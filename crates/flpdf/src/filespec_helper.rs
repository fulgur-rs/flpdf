//! qpdf correspondence: QPDFFileSpecObjectHelper.cc and QPDFEFStreamObjectHelper.cc.
//! Typed wrappers for `/Filespec` dictionaries and `/EmbeddedFile` streams,
//! plus a builder for constructing them.
//!
//! [`FileSpec`] wraps a `/Filespec` dictionary and exposes ergonomic, typed
//! accessors for all common fields (filename, description, embedded file
//! stream, etc.).  [`EmbeddedFileStream`] wraps the embedded `/EmbeddedFile`
//! stream reachable via the `/EF` sub-dictionary and exposes its payload and
//! metadata (MIME type, dates, checksum, size).
//!
//! [`FileSpecBuilder`] constructs a `/Filespec` dictionary and its associated
//! `/EmbeddedFile` stream in-memory and writes them into a [`Pdf`] document via
//! [`Pdf::set_object`].  The returned [`ObjectRef`] can then be inserted into
//! the `/Names /EmbeddedFiles` name tree using
//! [`crate::embedded_files::insert_embedded_file`].
//!
//! [`FileSpec`] and [`EmbeddedFileStream`] own qpdf-shaped object handles and
//! resolve dictionaries from the live document on each operation. Their
//! setters mutate those dictionary handles in place, including an indirect
//! `/Params` dictionary when one is present.
//!
//! # Design
//!
//! PDF key naming follows ISO 32000-1 §7.11.  The `/EF` lookup priority used
//! here mirrors qpdf's `QPDFFileSpecObjectHelper::name_keys` order
//! (`QPDFFileSpecObjectHelper.cc`), which is also what its `preferredcontents`
//! JSON output uses: `/UF` › `/F` › `/Unix` › `/DOS` › `/Mac`.
//!
//! Date strings (e.g. `/Params /CreationDate`) are returned as raw PDF date
//! byte sequences (`D:YYYYMMDDHHmmSSOHH'mm'`).  No date parsing is performed.
//!
//! # Examples
//!
//! ## Read filename and payload from a `/Filespec` object
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::{BufReader, Cursor};
//! use flpdf::{FileSpec, ObjectRef, Pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
//!
//! // Assume we know the /Filespec object reference (e.g. from walking /Names).
//! let filespec_ref = ObjectRef::new(5, 0);
//! let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
//!
//! if let Some(name) = fs.filename()? {
//!     println!("filename: {}", String::from_utf8_lossy(&name));
//! }
//! if let Some(mut ef) = fs.embedded_file()? {
//!     let bytes = ef.payload()?;
//!     println!("{} payload bytes", bytes.len());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Inspect embedded file metadata
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{FileSpec, ObjectRef, Pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
//! let filespec_ref = ObjectRef::new(5, 0);
//! let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), &mut pdf).unwrap();
//!
//! if let Some(mut ef) = fs.embedded_file()? {
//!     if let Some(mime) = ef.mimetype()? {
//!         println!("MIME: {}", String::from_utf8_lossy(&mime));
//!     }
//!     if let Some(created) = ef.creation_date()? {
//!         // raw PDF date string, e.g. b"D:20260101000000Z"
//!         println!("created: {}", String::from_utf8_lossy(&created));
//!     }
//!     if let Some(sz) = ef.size()? {
//!         println!("uncompressed size: {sz}");
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::filters::decode_stream_data;
use crate::object::{Dictionary, Object};
use crate::object_handle::{canonical_dictionary_key, StreamDataProvider};
use crate::pdf_string::{new_unicode_string, utf8_value};
use crate::pipeline::count::Count;
use crate::pipeline::md5::PlMd5;
use crate::pipeline::{Discard, Pipeline};
use crate::writer::DecodeLevel;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::rc::Rc;

const NAME_KEYS: [&str; 5] = ["UF", "F", "Unix", "DOS", "Mac"];

fn next_object_ref<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<ObjectRef> {
    let next = pdf
        .get_all_object_handles()?
        .iter()
        .filter_map(ObjectHandle::object_ref)
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(next, 0))
}

fn ensure_indirect_handle_belongs_to_pdf<R: Read + Seek>(
    handle: &ObjectHandle,
    pdf: &mut Pdf<R>,
    kind: &str,
) -> Result<()> {
    if handle.belongs_to_pdf(pdf.unique_id()) {
        Ok(())
    } else {
        Err(Error::Unsupported(format!(
            "{kind} handle belongs to another Pdf"
        )))
    }
}

// ── EmbeddedFileStream ────────────────────────────────────────────────────────

/// Wrapper for a `/EmbeddedFile` stream (ISO 32000-1 §7.11.4).
///
/// Construct via [`FileSpec::embedded_file`] rather than directly.
///
/// All accessors are cheap: only [`payload`](EmbeddedFileStream::payload)
/// performs I/O (decoding the filter chain).
pub struct EmbeddedFileStream<'a, R: Read + Seek + 'static> {
    /// qpdf's shared `/EmbeddedFile` object handle. Unlike a copied
    /// [`crate::Stream`], this preserves identity and lets metadata setters
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
    fn new_from_stream(pdf: &mut Pdf<R>, stream: ObjectHandle) -> Result<ObjectHandle> {
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
                let mut file = File::open(&path)?;
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
        let (stream, terminal_ref) = self
            .pdf
            .borrow_mut()
            .resolve_object_handle_to_terminal_ref(&self.stream)?;
        let stream = match terminal_ref {
            Some(object_ref) => {
                let mut pdf = self.pdf.borrow_mut();
                let stream = pdf.get_object_handle(object_ref);
                pdf.resolve_object_handle(&stream)?;
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
        Err(Error::System(format!(
            "operation for stream attempted on object of type {}",
            stream.type_name()
        )))
    }

    fn resolved_key(&self, dictionary: &ObjectHandle, key: &[u8]) -> Result<ObjectHandle> {
        let key = canonical_dictionary_key(key);
        let value = dictionary.get_key(&key);
        self.pdf
            .borrow_mut()
            .resolve_object_handle_to_terminal(&value)
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
        let dictionary = stream_dict
            .materialize()?
            .into_dict()
            .expect("stream dictionary handle must materialize as a dictionary");
        let data = stream.get_raw_stream_data()?;
        decode_stream_data(&dictionary, &data)
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
        let (resolved, terminal_ref) = self
            .pdf
            .borrow_mut()
            .resolve_object_handle_to_terminal_ref(&params)?;
        if resolved.as_dictionary().is_some() {
            let target = match terminal_ref {
                Some(object_ref) => {
                    let mut pdf = self.pdf.borrow_mut();
                    let target = pdf.get_object_handle(object_ref);
                    pdf.resolve_object_handle(&target)?;
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

// ── FileSpec ──────────────────────────────────────────────────────────────────

/// Wrapper for a `/Filespec` dictionary (ISO 32000-1 §7.11.3).
///
/// Construct with [`FileSpec::new`], passing the [`ObjectHandle`] of a
/// `/Filespec` dictionary and a mutable borrow of the open document.
///
/// All accessors except [`embedded_file`](FileSpec::embedded_file) are
/// cheap dictionary lookups that return `Ok(None)` when the key is absent.
/// [`embedded_file`](FileSpec::embedded_file) resolves the `/EF /F` (or `/EF /UF`) indirect reference.
pub struct FileSpec<'a, R: Read + Seek + 'static> {
    /// qpdf's Filespec object handle. It may be direct or indirect.
    filespec: ObjectHandle,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> FileSpec<'a, R> {
    /// Create an indirect `/Filespec` whose `/EF /F` and `/EF /UF` entries
    /// reference the same embedded-file stream, returning its object handle.
    ///
    /// This is the Rust form of qpdf's `createFileSpec` helper factory.
    pub fn create_file_spec(
        pdf: &mut Pdf<R>,
        filename: impl AsRef<[u8]>,
        embedded_file: ObjectHandle,
    ) -> Result<ObjectHandle> {
        let name = new_unicode_string(filename.as_ref());
        let mut ef = Dictionary::new();
        let embedded_file = if embedded_file.object_ref().is_some() {
            // qpdf's `QPDFObjectHandle::checkOwnership` compares the owning
            // QPDF of the value being inserted. It does not look up that
            // value by object number in the destination, since doing so would
            // register a foreign reference and alter subsequent allocation.
            if !pdf.is_canonical_object_handle(&embedded_file) {
                return Err(Error::Unsupported(
                    "embedded-file handle belongs to another Pdf".to_string(),
                ));
            }
            embedded_file
        } else {
            pdf.make_indirect_object_handle(embedded_file)?
        };
        let embedded_file = Object::Reference(
            embedded_file
                .object_ref()
                .expect("make_indirect_object_handle must return an indirect handle"),
        );
        ef.insert("F", embedded_file.clone());
        ef.insert("UF", embedded_file);

        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"Filespec".to_vec()));
        dict.insert("UF", Object::String(name.clone()));
        dict.insert("F", Object::String(name));
        dict.insert("EF", Object::Dictionary(ef));

        let object_ref = next_object_ref(pdf)?;
        pdf.set_object(object_ref, Object::Dictionary(dict));
        Ok(pdf.get_object_handle(object_ref))
    }

    /// Create a Filespec and embedded-file stream from a filesystem path.
    pub fn create_file_spec_from_path<P: AsRef<Path>>(
        pdf: &mut Pdf<R>,
        filename: impl AsRef<[u8]>,
        path: P,
    ) -> Result<ObjectHandle> {
        let embedded_file = EmbeddedFileStream::create_ef_stream_from_path(pdf, path)?;
        Self::create_file_spec(pdf, filename, embedded_file)
    }

    /// Construct a new wrapper for a `/Filespec` dictionary handle.
    ///
    /// The handle may be direct or indirect, matching qpdf's
    /// `QPDFFileSpecObjectHelper(QPDFObjectHandle)` constructor. An indirect
    /// handle must be the canonical handle of `pdf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for an indirect handle owned by another
    /// [`Pdf`], preventing a same-number object in `pdf` from being selected.
    pub fn new(filespec: ObjectHandle, pdf: &'a mut Pdf<R>) -> Result<Self> {
        ensure_indirect_handle_belongs_to_pdf(&filespec, pdf, "Filespec")?;
        // QPDFFileSpecObjectHelper.cc:10-18 warns at construction time for a
        // non-dictionary value or a dictionary with the wrong /Type.
        let resolved = pdf.resolve_object_handle_to_terminal(&filespec)?;
        if resolved.try_as_dictionary()?.is_none() {
            resolved.warn_if_possible("Embedded file object is not a dictionary")?;
        } else if !resolved.try_is_dictionary_of_type(b"Filespec", b"")? {
            resolved.warn_if_possible("Embedded file object's type is not /Filespec")?;
        }
        Ok(Self { filespec, pdf })
    }

    /// Resolve the helper's object in place and retain its qpdf description.
    /// Accessors use this handle directly so `try_get_key` can emit the same
    /// `typeWarning` that qpdf's `QPDFObjectHandle::getKey` emits for a
    /// non-dictionary Filespec.
    fn filespec_handle(&mut self) -> Result<ObjectHandle> {
        let (filespec, terminal_ref) = self
            .pdf
            .resolve_object_handle_to_terminal_ref(&self.filespec)?;
        Ok(match terminal_ref {
            Some(object_ref) => {
                let filespec = self.pdf.get_object_handle(object_ref);
                self.pdf.resolve_object_handle(&filespec)?;
                filespec
            }
            None => filespec,
        })
    }

    fn filespec_value(&mut self, key: &[u8]) -> Result<ObjectHandle> {
        let filespec = self.filespec_handle()?;
        if filespec.is_null() && filespec.context().is_none() {
            return Ok(ObjectHandle::null());
        }
        filespec.try_get_key(key)
    }

    fn resolved_string_value_handle(&mut self, value: ObjectHandle) -> Result<Option<Vec<u8>>> {
        let value = self.pdf.resolve_object_handle_to_terminal(&value)?;
        Ok(value.as_string())
    }

    fn filespec_dict(&mut self) -> Result<Option<ObjectHandle>> {
        let filespec = self.filespec_handle()?;
        Ok(filespec.as_dictionary().map(|_| filespec))
    }

    /// Resolve the `/Filespec` dictionary. qpdf treats a non-dictionary
    /// helper object as its null dictionary, so callers receive their usual
    /// empty-value defaults rather than a type error.
    fn resolve_dict(&mut self) -> Result<Option<Dictionary>> {
        let Some(dictionary) = self.filespec_dict()? else {
            return Ok(None);
        };
        let dict = dictionary
            .materialize()?
            .into_dict()
            .expect("dictionary handle must materialize as a dictionary");
        Ok(Some(dict))
    }

    /// Set `/Desc` with qpdf's `newUnicodeString` storage semantics.
    pub fn set_description(&mut self, description: impl AsRef<[u8]>) -> Result<&mut Self> {
        let Some(dict) = self.filespec_dict()? else {
            return Ok(self);
        };
        self.pdf.mark_object_handle_dirty(&dict)?;
        dict.replace_key(
            b"/Desc",
            ObjectHandle::string(new_unicode_string(description.as_ref())),
        )?; // cov:ignore: FileSpec::new validates the receiver's document ownership
        Ok(self)
    }

    /// Set `/UF` and `/F` with qpdf's compatibility-filename behavior.
    ///
    /// `unicode_name` and a non-empty `compatibility_name` are byte sequences,
    /// matching qpdf's `std::string` parameters. The Unicode value is stored
    /// with `newUnicodeString`; the compatibility value is stored verbatim.
    pub fn set_filename(
        &mut self,
        unicode_name: impl AsRef<[u8]>,
        compatibility_name: Option<&[u8]>,
    ) -> Result<&mut Self> {
        let Some(dict) = self.filespec_dict()? else {
            return Ok(self);
        };
        self.pdf.mark_object_handle_dirty(&dict)?;
        let unicode_name = new_unicode_string(unicode_name.as_ref());
        dict.replace_key(b"/UF", ObjectHandle::string(unicode_name.clone()))?;
        let compatibility_name = compatibility_name
            .map(ToOwned::to_owned)
            .filter(|name| !name.is_empty());
        dict.replace_key(
            b"/F",
            ObjectHandle::string(compatibility_name.unwrap_or(unicode_name)),
        )?; // cov:ignore: FileSpec::new validates the receiver's document ownership
        Ok(self)
    }

    /// Return `/F` — the file name as raw PDF string bytes.
    ///
    /// Returns `None` when the key is absent or the value is not a string.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn filename(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.resolve_dict()?.and_then(|dict| {
            dict.get("F")
                .and_then(Object::as_string)
                .map(ToOwned::to_owned)
        }))
    }

    /// Return `/UF` — the Unicode-encoded file name as raw PDF string bytes.
    ///
    /// `/UF` contains a UTF-16BE (with BOM) or PDFDocEncoding string.  The
    /// raw bytes are returned without decoding — callers may apply their own
    /// text-string decoder if needed.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn uf(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.resolve_dict()?.and_then(|dict| {
            dict.get("UF")
                .and_then(Object::as_string)
                .map(ToOwned::to_owned)
        }))
    }

    /// Return `/Desc` — the file description as raw PDF string bytes.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn description(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.resolve_dict()?.and_then(|dict| {
            dict.get("Desc")
                .and_then(Object::as_string)
                .map(ToOwned::to_owned)
        }))
    }

    /// Return `/AFRelationship` — the associated-file relationship as raw
    /// PDF name bytes (e.g. `b"Source"`, `b"Data"`, `b"Alternative"`).
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn af_relationship(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.resolve_dict()?.and_then(|dict| {
            dict.get("AFRelationship")
                .and_then(Object::as_name)
                .map(ToOwned::to_owned)
        }))
    }

    /// Return `/Desc` through qpdf's UTF-8 string view.
    ///
    /// The byte vector mirrors qpdf's `std::string`: an explicit UTF-8 BOM
    /// may be followed by invalid UTF-8, which Rust's [`String`] cannot hold.
    pub fn get_description(&mut self) -> Result<Vec<u8>> {
        let value = self.filespec_value(b"/Desc")?;
        Ok(self
            .resolved_string_value_handle(value)?
            .map(|value| utf8_value(&value))
            .unwrap_or_default())
    }

    /// Return the preferred file name using qpdf's `/UF`, `/F`, `/Unix`,
    /// `/DOS`, `/Mac` priority order and UTF-8 value conversion.
    pub fn get_filename(&mut self) -> Result<Vec<u8>> {
        for key in NAME_KEYS {
            let key = canonical_dictionary_key(key.as_bytes());
            let value = self.filespec_value(&key)?;
            if let Some(value) = self.resolved_string_value_handle(value)? {
                return Ok(utf8_value(&value));
            }
        }
        Ok(Vec::new())
    }

    /// Return every recognized Filespec name key whose value is a string.
    ///
    /// Keys retain qpdf's leading slash, e.g. `"/UF"`.
    pub fn get_filenames(&mut self) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut filenames = BTreeMap::new();
        for name_key in NAME_KEYS {
            let key = canonical_dictionary_key(name_key.as_bytes());
            let value = self.filespec_value(&key)?;
            if let Some(value) = self.resolved_string_value_handle(value)? {
                filenames.insert(format!("/{name_key}"), utf8_value(&value));
            }
        }
        Ok(filenames)
    }

    /// Return the raw `/EF` entry for `key`, or qpdf's null-object equivalent
    /// when `/EF` or the requested key is absent.
    ///
    /// An empty `key` performs qpdf's preferred stream lookup: it skips
    /// non-stream candidates and returns the first candidate that resolves to
    /// a stream, preserving the original reference when it was indirect.
    pub fn get_embedded_file_stream(&mut self, key: &str) -> Result<ObjectHandle> {
        let ef = self.get_embedded_file_streams()?;
        let ef = self.pdf.resolve_object_handle_to_terminal(&ef)?;
        let Some(entries) = ef.try_as_dictionary()? else {
            return Ok(ObjectHandle::null());
        };

        if !key.is_empty() {
            let key = canonical_dictionary_key(key.as_bytes());
            return ef.try_get_key(&key);
        }

        for key in NAME_KEYS {
            let key = canonical_dictionary_key(key.as_bytes());
            let Some(candidate) = entries.get(&key).cloned() else {
                continue;
            };
            let terminal = self.pdf.resolve_object_handle_to_terminal(&candidate)?;
            if terminal.as_stream_dict().is_some() {
                return Ok(candidate);
            }
        }
        Ok(ObjectHandle::null())
    }

    /// Return the raw `/EF` dictionary, or qpdf's null-object equivalent when
    /// the key is absent.
    pub fn get_embedded_file_streams(&mut self) -> Result<ObjectHandle> {
        self.filespec_value(b"/EF")
    }

    /// Return the raw `/EF` dictionary items in qpdf's `ditems()` order.
    ///
    /// `QPDF_Dictionary::ditems()` asks the receiver for its key set at both
    /// iterator construction and end comparison.  Retaining both calls is
    /// observable for a missing `/EF`: qpdf emits two `typeWarning`s while
    /// treating its null object as an empty dictionary.
    pub(crate) fn get_embedded_file_stream_entries(
        &mut self,
    ) -> Result<Vec<(Vec<u8>, ObjectHandle)>> {
        let ef = self.get_embedded_file_streams()?;
        if ef.is_null() && ef.context().is_none() {
            return Ok(Vec::new());
        }
        let keys = ef.try_get_keys()?;
        let _end_keys = ef.try_get_keys()?;
        keys.into_iter()
            .map(|key| Ok((key.clone(), ef.try_get_key(&key)?)))
            .collect()
    }

    /// Resolve and return the embedded file stream.
    ///
    /// The lookup priority for the `/EF` sub-dictionary key is
    /// `/UF`, `/F`, `/Unix`, `/DOS`, `/Mac` — the same preference order
    /// qpdf applies (Unicode name first), consistent with ISO 32000-1
    /// §7.11.4.  The first key that resolves to an `/EmbeddedFile` stream
    /// reference is used.
    ///
    /// Returns `Ok(None)` when the `/Filespec` dictionary has no `/EF` entry
    /// or when none of the standard keys (`/UF`, `/F`, `/Unix`, `/DOS`,
    /// `/Mac`) resolve to an `/EmbeddedFile` stream.
    ///
    /// A candidate key whose value is not an indirect reference, or that
    /// resolves to a non-stream object, is skipped and the search continues
    /// with the next key; if no key yields an `/EmbeddedFile` stream the
    /// method returns `Ok(None)` (it does not error on a non-stream entry).
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when the `/Filespec` object itself is not a
    ///   dictionary.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use flpdf::{FileSpec, ObjectRef, Pdf};
    /// # use std::fs::File;
    /// # use std::io::BufReader;
    /// # let mut pdf = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
    /// let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    /// if let Some(mut ef) = fs.embedded_file()? {
    ///     let bytes = ef.payload()?;
    ///     println!("{} bytes", bytes.len());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn embedded_file(&mut self) -> Result<Option<EmbeddedFileStream<'_, R>>> {
        let candidate = self.get_embedded_file_stream("")?;
        let (stream, terminal_ref) = self.pdf.resolve_object_handle_to_terminal_ref(&candidate)?;
        let Some(stream) = stream.as_stream_dict().map(|_| stream) else {
            return Ok(None);
        };
        let stream = terminal_ref
            .map(|object_ref| self.pdf.get_object_handle(object_ref))
            .unwrap_or(stream);
        Ok(Some(EmbeddedFileStream::new(stream, self.pdf)?))
    }
}

// ── Encoding helpers ──────────────────────────────────────────────────────────

/// Encode a Unicode filename as a UTF-16BE string with BOM.
///
/// The returned bytes are: `[0xFE, 0xFF]` (BOM) followed by each UTF-16BE
/// code unit as two big-endian bytes.  This matches the `/UF` encoding
/// required by ISO 32000-1 §7.11.3.
///
/// # Examples
///
/// ```
/// use flpdf::filespec_helper::encode_utf16be;
///
/// let bytes = encode_utf16be("hi");
/// // BOM + 'h' (0x0068) + 'i' (0x0069)
/// assert_eq!(bytes, vec![0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69]);
/// ```
pub fn encode_utf16be(s: &str) -> Vec<u8> {
    let mut out = vec![0xFE_u8, 0xFF]; // BOM
    for unit in s.encode_utf16() {
        out.push((unit >> 8) as u8);
        out.push((unit & 0xFF) as u8);
    }
    out
}

/// Format a date tuple `(year, month, day, hour, minute, second)` as a PDF
/// date string: `D:YYYYMMDDHHmmSSZ`.
///
/// The timezone suffix is always `Z` (UTC).  No validation of the individual
/// fields is performed.
///
/// # Examples
///
/// ```
/// use flpdf::filespec_helper::format_pdf_date;
///
/// assert_eq!(format_pdf_date(2026, 1, 1, 0, 0, 0), b"D:20260101000000Z".to_vec());
/// assert_eq!(format_pdf_date(2025, 12, 31, 23, 59, 59), b"D:20251231235959Z".to_vec());
/// ```
pub fn format_pdf_date(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Vec<u8> {
    format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}Z",
        year, month, day, hour, minute, second
    )
    .into_bytes()
}

// NOTE: a public `escape_pdf_name` helper used to live here. It was removed
// (roborev #920): `Object::Name` holds *decoded* logical bytes and the
// serializer escapes delimiters on write (#919), so escaping before
// constructing `Object::Name` would double-escape (`#` → `#23`). Callers must
// pass raw logical bytes straight to `Object::Name`. The canonical escaper now
// lives at `crate::object::escape_name_bytes` and is serializer-internal.

/// Compute the MD5 checksum of `data` and return it as a 16-byte `Vec<u8>`.
///
/// This is the checksum stored in `/Params /CheckSum` (ISO 32000-1 §7.11.4).
pub fn md5_checksum(data: &[u8]) -> Vec<u8> {
    let mut discard = Discard;
    let mut md5 = PlMd5::new("EF md5", &mut discard);
    md5.write(data)
        .expect("embedded-file MD5 discard write is infallible");
    md5.finish()
        .expect("embedded-file MD5 discard finish is infallible");
    let hex_digest = md5
        .get_hex_digest()
        .expect("embedded-file MD5 pipeline remains enabled");
    hex::decode(hex_digest).expect("PlMd5 always returns lowercase hexadecimal")
}

// ── FileSpecBuilder ───────────────────────────────────────────────────────────

/// Optional date fields for a `/Params` sub-dictionary.
///
/// Each tuple is `(year, month, day, hour, minute, second)`.
#[derive(Debug, Clone, Default)]
pub struct FileParamDates {
    /// `/Params /CreationDate` as `(year, month, day, hour, minute, second)`.
    pub creation: Option<(u16, u8, u8, u8, u8, u8)>,
    /// `/Params /ModDate` as `(year, month, day, hour, minute, second)`.
    pub modification: Option<(u16, u8, u8, u8, u8, u8)>,
}

/// Builder that constructs a `/Filespec` dictionary and its associated
/// `/EmbeddedFile` stream, then inserts both into a [`Pdf`] document.
///
/// Use [`FileSpecBuilder::new`] to create a builder, configure it with the
/// setter methods, then call [`FileSpecBuilder::build`] to write the objects
/// and obtain the filespec [`ObjectRef`].
///
/// # Example
///
/// ```no_run
/// # use flpdf::{filespec_helper::FileSpecBuilder, embedded_files, Pdf};
/// # use std::io::{BufReader, Cursor};
/// # let mut pdf: Pdf<Cursor<Vec<u8>>> = todo!();
/// let filespec_ref = FileSpecBuilder::new("report.pdf", b"...pdf bytes...")
///     .mimetype(b"application/pdf")
///     .description(b"Annual report")
///     .af_relationship(b"Data")
///     .build(&mut pdf)
///     .expect("build filespec");
/// embedded_files::insert_embedded_file(&mut pdf, b"report.pdf", filespec_ref)
///     .expect("insert into name tree");
/// ```
pub struct FileSpecBuilder {
    /// ASCII filename used for `/F`.
    filename: Vec<u8>,
    /// Unicode filename used for `/UF`; defaults to `filename` interpreted as UTF-8.
    uf_filename: Option<String>,
    /// Raw payload bytes for the `/EmbeddedFile` stream (uncompressed).
    payload: Vec<u8>,
    /// MIME type stored in `/EmbeddedFile /Subtype` (raw, e.g. `b"application/pdf"`).
    mimetype: Option<Vec<u8>>,
    /// Human-readable description stored in `/Filespec /Desc`.
    description: Option<Vec<u8>>,
    /// Associated-file relationship stored in `/Filespec /AFRelationship`.
    af_relationship: Option<Vec<u8>>,
    /// Optional date metadata for the `/Params` sub-dictionary.
    dates: FileParamDates,
}

impl FileSpecBuilder {
    /// Create a new builder for a file with the given ASCII `filename` and
    /// raw `payload` bytes.
    ///
    /// `filename` is stored verbatim in `/F`. `/UF` is encoded with qpdf's
    /// `newUnicodeString` behavior: it uses PDFDocEncoding when every
    /// character is representable and otherwise uses UTF-16BE with a BOM.
    /// For non-ASCII filenames, construct the builder with an ASCII fallback
    /// for `/F` and call [`Self::uf_filename`] with the original Unicode name.
    ///
    /// `payload` must be the **decoded** (uncompressed) bytes.  By default the
    /// builder writes them verbatim to the stream (no `/Filter`), matching
    /// qpdf's `QPDFEFStreamObjectHelper::createEFStream` factory.
    pub fn new(filename: impl AsRef<[u8]>, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            filename: filename.as_ref().to_vec(),
            uf_filename: None,
            payload: payload.into(),
            mimetype: None,
            description: None,
            af_relationship: None,
            dates: FileParamDates::default(),
        }
    }

    /// Set the Unicode filename stored in `/UF`, independently from `/F`.
    ///
    /// Use this when `/F` must be an ASCII-safe fallback but `/UF` should
    /// preserve the original Unicode filename.
    pub fn uf_filename(mut self, filename: impl AsRef<str>) -> Self {
        self.uf_filename = Some(filename.as_ref().to_string());
        self
    }

    /// Set the MIME type (stored in `/EmbeddedFile /Subtype`).
    ///
    /// `mime` should be the raw MIME type bytes, e.g. `b"application/pdf"`.
    /// The builder will escape `/` and other PDF delimiter bytes using `#XX`
    /// notation so that the name token is valid PDF syntax and round-trips
    /// correctly through the parser.
    pub fn mimetype(mut self, mime: impl AsRef<[u8]>) -> Self {
        self.mimetype = Some(mime.as_ref().to_vec());
        self
    }

    /// Set the file description (stored in `/Filespec /Desc`).
    pub fn description(mut self, desc: impl AsRef<[u8]>) -> Self {
        self.description = Some(desc.as_ref().to_vec());
        self
    }

    /// Set the `/AFRelationship` name (e.g. `b"Source"`, `b"Data"`).
    pub fn af_relationship(mut self, rel: impl AsRef<[u8]>) -> Self {
        self.af_relationship = Some(rel.as_ref().to_vec());
        self
    }

    /// Set the creation and/or modification dates for `/Params`.
    pub fn dates(mut self, dates: FileParamDates) -> Self {
        self.dates = dates;
        self
    }

    /// Build the `/Filespec` and `/EmbeddedFile` objects and insert them into
    /// `pdf`.  Returns the [`ObjectRef`] of the `/Filespec` dictionary.
    ///
    /// Two new indirect objects are allocated:
    /// - One `/EmbeddedFile` stream containing the payload.
    /// - One `/Filespec` dictionary pointing to the stream via `/EF`.
    ///
    /// The caller is responsible for inserting the returned ref into the
    /// document's `/Names /EmbeddedFiles` name tree.
    ///
    /// # Errors
    ///
    /// Returns an error only if object-number allocation fails (in practice
    /// this cannot happen with a well-formed document).
    pub fn build<R: Read + Seek>(self, pdf: &mut Pdf<R>) -> Result<ObjectRef> {
        let uf_filename = match self.uf_filename {
            Some(filename) => filename,
            None => std::str::from_utf8(&self.filename)
                .map_err(|_| {
                    Error::Unsupported(
                        "FileSpecBuilder: filename is not valid UTF-8; cannot encode /UF"
                            .to_string(),
                    )
                })?
                .to_string(),
        };

        // The two qpdf-shaped factories own all base dictionary construction:
        // `/Type`, `/Params /Size`, `/Params /CheckSum`, `/F`, `/UF`, and the
        // paired `/EF` references. The builder adds only its opt-in features.
        let stream_handle = EmbeddedFileStream::create_ef_stream(pdf, &self.payload)?;
        let filespec_handle = FileSpec::create_file_spec(pdf, &uf_filename, stream_handle)?;
        let filespec_ref = filespec_handle
            .object_ref()
            .expect("create_file_spec must create an indirect Filespec");
        {
            let mut filespec = FileSpec::new(pdf.get_object_handle(filespec_ref), pdf)?;
            filespec.set_filename(&uf_filename, Some(self.filename.as_slice()))?;
            if let Some(description) = self.description {
                filespec.set_description(description)?;
            }

            let mut embedded_file = filespec
                .embedded_file()?
                .expect("FileSpec::create must create an /EmbeddedFile stream");
            if let Some(mimetype) = self.mimetype {
                embedded_file.set_subtype(mimetype)?;
            }
            if let Some((y, mo, d, h, mi, s)) = self.dates.creation {
                embedded_file.set_creation_date(format_pdf_date(y, mo, d, h, mi, s))?;
            }
            if let Some((y, mo, d, h, mi, s)) = self.dates.modification {
                embedded_file.set_mod_date(format_pdf_date(y, mo, d, h, mi, s))?;
            }
        }

        if let Some(relationship) = self.af_relationship {
            let Object::Dictionary(mut filespec) = pdf.resolve(filespec_ref)? else {
                unreachable!("FileSpec::create must create a dictionary"); // cov:ignore: factory return type makes this arm unreachable
            };
            filespec.insert("AFRelationship", Object::Name(relationship));
            pdf.set_object(filespec_ref, Object::Dictionary(filespec));
        }

        Ok(filespec_ref)
    }
}

// ── High-level attachment helper ──────────────────────────────────────────────

/// Attach a file from disk to `pdf` through qpdf's filesystem provider path.
///
/// This is a convenience wrapper around [`FileSpec::create_file_spec_from_path`] +
/// [`crate::embedded_files::insert_embedded_file`] that:
///
/// 1. Streams the file at `path` through the deferred provider factory.
/// 2. Derives the name-tree key and `/F`/`/UF` filename from the path's
///    **basename** (the last component of the path).
/// 3. Builds a `/Filespec` + `/EmbeddedFile` pair without installing a local
///    filter. `/Params /Size` and `/Params /CheckSum` reflect the **raw** bytes,
///    as required by ISO 32000-1 §7.11.4.
/// 4. Inserts the pair into the catalog's `/Names /EmbeddedFiles` name tree
///    under the UTF-8 `key` (which may differ from the basename if the caller
///    wants an explicit tree key).
///
/// Returns the [`ObjectRef`] of the newly created `/Filespec` dictionary.
///
/// # Parameters
///
/// - `pdf` — the target document (must be mutable).
/// - `key` — the name-tree key used to look up the attachment later (e.g. the
///   basename encoded as bytes, or any other agreed-upon string).
/// - `path` — path to the file on disk; its basename is used for `/F`/`/UF`.
///
/// # Errors
///
/// - [`Error::Io`] if the file cannot be opened or read.
/// - [`Error::Unsupported`] if the path has no basename or the basename is not
///   valid UTF-8.
/// - Any error from [`FileSpec::create_file_spec_from_path`] or
///   [`crate::embedded_files::insert_embedded_file`].
///
/// # Example
///
/// ```no_run
/// use std::io::Cursor;
/// use flpdf::{filespec_helper, Pdf};
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf: Pdf<Cursor<Vec<u8>>> = todo!();
/// let fs_ref = filespec_helper::add_attachment_from_path(
///     &mut pdf,
///     b"README.txt",
///     "/tmp/README.txt",
/// )?;
/// println!("inserted filespec at {fs_ref}");
/// # Ok(())
/// # }
/// ```
pub fn add_attachment_from_path<R, P>(pdf: &mut Pdf<R>, key: &[u8], path: P) -> Result<ObjectRef>
where
    R: Read + Seek,
    P: AsRef<Path>,
{
    let path = path.as_ref();

    // Derive the basename for /F and /UF.
    let basename = path
        .file_name()
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "add_attachment_from_path: path has no basename: {}",
                path.display()
            ))
        })?
        .to_str()
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "add_attachment_from_path: basename is not valid UTF-8: {}",
                path.display()
            ))
        })?;

    // Build the /Filespec + /EmbeddedFile through qpdf's path-provider route.
    // `create_file_spec` initially uses the same Unicode name for `/F` and
    // `/UF`; replace `/F` with the independent ASCII fallback while retaining
    // the original Unicode `/UF` value, matching FileSpecBuilder's behavior.
    let filespec_handle = FileSpec::create_file_spec_from_path(pdf, basename.as_bytes(), path)?;
    let filespec_ref = filespec_handle
        .object_ref()
        .expect("create_file_spec_from_path must create an indirect Filespec");
    let fallback = ascii_filename_fallback(basename);
    {
        let mut filespec = FileSpec::new(pdf.get_object_handle(filespec_ref), pdf)?;
        filespec.set_filename(basename.as_bytes(), Some(fallback.as_slice()))?;
    }
    crate::embedded_files::insert_embedded_file(pdf, key, filespec_ref)?;

    Ok(filespec_ref)
}

/// Return an ASCII-safe `/F` fallback while preserving readable ASCII filename parts.
pub fn ascii_filename_fallback(filename: &str) -> Vec<u8> {
    let fallback: String = filename
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if fallback.is_empty() || fallback.bytes().all(|b| b == b'.' || b == b'_') {
        b"attachment".to_vec()
    } else {
        fallback.into_bytes()
    }
}

// ── Attachment extraction API ─────────────────────────────────────────────────

/// Extract the decoded payload of an attachment identified by `key`.
///
/// Looks up `key` in the catalog's `/Names /EmbeddedFiles` name tree, resolves
/// the associated `/Filespec` dictionary, and decodes the `/EmbeddedFile` stream
/// (applying the filter chain, e.g. FlateDecode) to return the original file
/// contents.
///
/// # Note on direct-dict filespecs
///
/// Name-tree entries whose value is a direct `/Filespec` dictionary (rather than
/// an indirect reference) are not surfaced by the underlying
/// [`crate::embedded_files::list_embedded_files`] enumeration; they are
/// skipped with the same limitation documented there. Only attachments with
/// indirect-reference values are extractable by this function.
///
/// # Errors
///
/// - [`Error::Unsupported`] when `key` is not present in the name tree.  The
///   error message includes the missing key name and a sorted list of available
///   keys so the caller can emit an actionable diagnostic.
/// - [`Error::Unsupported`] when the filespec at `key` has no resolvable
///   `/EmbeddedFile` stream (e.g. the `/EF` sub-dictionary is absent or
///   malformed).
/// - Any error from [`Pdf::resolve`] or the filter decoder.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{filespec_helper, Pdf};
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// let bytes = filespec_helper::extract_attachment(&mut pdf, b"report.pdf")?;
/// println!("extracted {} bytes", bytes.len());
/// # Ok(())
/// # }
/// ```
pub fn extract_attachment<R: Read + Seek>(pdf: &mut Pdf<R>, key: &[u8]) -> Result<Vec<u8>> {
    // Look up all entries in the name tree.
    let entries = crate::embedded_files::list_embedded_files(pdf)?;

    // Find the target key.
    let filespec_ref = match entries.iter().find(|(k, _)| k.as_slice() == key) {
        Some((_, r)) => *r,
        None => {
            // Collect available keys for an actionable error message.
            // Sorted so the diagnostic is deterministic / reproducible,
            // independent of name-tree iteration order (CodeRabbit nitpick).
            let mut available: Vec<String> = entries
                .iter()
                .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                .collect();
            available.sort_unstable();
            let hint = if available.is_empty() {
                " (no attachments present)".to_string()
            } else {
                format!(" (available keys: {})", available.join(", "))
            };
            return Err(Error::Unsupported(format!(
                "extract_attachment: key {:?} not found{}",
                String::from_utf8_lossy(key),
                hint,
            )));
        }
    };

    // Resolve the filespec and decode its embedded file stream.
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), pdf)?;
    let ef = fs.embedded_file()?.ok_or_else(|| {
        Error::Unsupported(format!(
            "extract_attachment: key {:?} has no resolvable /EmbeddedFile stream \
             (the /EF sub-dictionary may be absent or malformed)",
            String::from_utf8_lossy(key),
        ))
    })?;
    ef.payload()
}

/// Write the decoded payload of attachment `key` to `out`.
///
/// Decodes the embedded file stream via [`extract_attachment`] and writes all
/// bytes to `out` in a single [`Write::write_all`] call.
///
/// # Errors
///
/// Propagates all errors from [`extract_attachment`] and from `out.write_all`.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{filespec_helper, Pdf};
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// let mut buf = Vec::new();
/// filespec_helper::write_attachment(&mut pdf, b"report.pdf", &mut buf)?;
/// println!("wrote {} bytes", buf.len());
/// # Ok(())
/// # }
/// ```
pub fn write_attachment<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    key: &[u8],
    out: &mut W,
) -> Result<()> {
    let bytes = extract_attachment(pdf, key)?;
    out.write_all(&bytes)?;
    Ok(())
}

/// Write the decoded payload of attachment `key` to a file at `path`.
///
/// Creates (or truncates) the file at `path` and writes the decoded stream
/// bytes.  This is the library-side counterpart of the CLI `-o` option
/// (wiring of the `-o` flag is handled by the CLI layer, not here).
///
/// # Errors
///
/// - Any error from [`extract_attachment`].
/// - [`Error::Io`] if the file cannot be created or written.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{filespec_helper, Pdf};
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// filespec_helper::extract_attachment_to_path(&mut pdf, b"report.pdf", "/tmp/out.pdf")?;
/// # Ok(())
/// # }
/// ```
pub fn extract_attachment_to_path<R, P>(pdf: &mut Pdf<R>, key: &[u8], path: P) -> Result<()>
where
    R: Read + Seek,
    P: AsRef<Path>,
{
    let bytes = extract_attachment(pdf, key)?;
    std::fs::write(path, &bytes)?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded_files::{insert_embedded_file, list_embedded_files};
    use crate::filters::decode_stream_data;
    use crate::{Object, ObjectRef, Pdf};
    use std::io::Cursor;

    // ── Minimal PDF fixture ───────────────────────────────────────────────────

    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn open_minimal() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open minimal PDF")
    }

    #[test]
    fn builder_rejects_a_non_utf8_filename_without_a_unicode_override() {
        let mut pdf = open_minimal();
        let error = FileSpecBuilder::new(b"\xff.txt", b"payload".as_slice())
            .build(&mut pdf)
            .expect_err("/UF requires an explicit Unicode filename for non-UTF-8 bytes");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: FileSpecBuilder: filename is not valid UTF-8; cannot encode /UF"
        );
    }

    #[test]
    fn embedded_file_finalizer_rejects_a_non_stream_handle() {
        let mut pdf = open_minimal();
        let error = EmbeddedFileStream::new_from_stream(&mut pdf, ObjectHandle::null())
            .expect_err("the shared finalizer requires a stream handle");

        assert_eq!(
            error.to_string(),
            "EmbeddedFile factory received a non-stream object"
        );
    }

    #[test]
    fn filespec_helper_chases_a_reference_holder_to_the_terminal_dictionary() {
        let mut pdf = open_minimal();
        let filespec_ref = ObjectRef::new(5, 0);
        let holder_ref = ObjectRef::new(6, 0);
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"terminal.txt".to_vec()));
        pdf.set_object(filespec_ref, Object::Dictionary(filespec));
        pdf.set_object(holder_ref, Object::Reference(filespec_ref));

        let mut helper = FileSpec::new(pdf.get_object_handle(holder_ref), &mut pdf).unwrap();
        assert_eq!(helper.get_filename().unwrap(), b"terminal.txt");
        helper.set_description("terminal description").unwrap();
        drop(helper);

        let filespec = pdf
            .resolve(filespec_ref)
            .unwrap()
            .into_dict()
            .expect("terminal object must be a Filespec dictionary");
        assert_eq!(
            filespec.get("Desc"),
            Some(&Object::String(b"terminal description".to_vec()))
        );
    }

    #[test]
    fn direct_null_filespec_stream_entries_are_empty() {
        let mut pdf = open_minimal();
        let mut helper = FileSpec::new(ObjectHandle::null(), &mut pdf).unwrap();

        assert!(helper
            .get_embedded_file_stream_entries()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn filespec_helper_marks_an_indirect_owner_of_a_direct_dictionary_dirty() {
        let mut pdf = open_minimal();
        let owner_ref = ObjectRef::new(5, 0);
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"direct.txt".to_vec()));
        let mut owner = Dictionary::new();
        owner.insert("FS", Object::Dictionary(filespec));
        pdf.set_object(owner_ref, Object::Dictionary(owner));
        let owner = pdf.get_object_handle(owner_ref);
        pdf.resolve_object_handle(&owner).unwrap();
        let direct_filespec = owner.get_key(b"/FS");
        pdf.clear_dirty(owner_ref);

        let mut helper = FileSpec::new(direct_filespec, &mut pdf).unwrap();
        helper.set_description("persisted through owner").unwrap();
        drop(helper);

        assert!(pdf.is_dirty(owner_ref));
    }

    #[test]
    fn helper_constructors_reject_indirect_handles_from_another_pdf() {
        let mut source = open_minimal();
        let foreign_filespec = source.get_object_handle(ObjectRef::new(1, 0));
        let foreign_stream = source.get_object_handle(ObjectRef::new(2, 0));
        let mut destination = open_minimal();

        assert!(FileSpec::new(foreign_filespec, &mut destination).is_err());
        assert!(EmbeddedFileStream::new(foreign_stream, &mut destination).is_err());
    }

    #[test]
    fn filespec_constructor_rejects_a_direct_child_from_another_pdf() {
        let mut source = open_minimal();
        let owner_ref = ObjectRef::new(5, 0);
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"foreign.txt".to_vec()));
        let mut owner_dict = Dictionary::new();
        owner_dict.insert("FS", Object::Dictionary(filespec));
        source.set_object(owner_ref, Object::Dictionary(owner_dict));
        let owner = source.get_object_handle(owner_ref);
        source.resolve_object_handle(&owner).unwrap();
        let foreign_direct_filespec = owner.get_key(b"/FS");
        assert!(foreign_direct_filespec.is_direct());

        let mut destination = open_minimal();
        let error = FileSpec::new(foreign_direct_filespec, &mut destination)
            .err()
            .expect("a direct child owned by another Pdf must be rejected");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: Filespec handle belongs to another Pdf"
        );
    }

    #[test]
    fn rejecting_a_foreign_handle_does_not_reserve_its_object_number() {
        let mut source = open_minimal();
        let foreign = source.get_object_handle(ObjectRef::new(99, 0));
        let mut destination = open_minimal();

        assert!(FileSpec::new(foreign, &mut destination).is_err());
        assert_eq!(
            EmbeddedFileStream::create_ef_stream(&mut destination, b"payload")
                .unwrap()
                .object_ref(),
            Some(ObjectRef::new(4, 0))
        );
    }

    #[test]
    fn create_filespec_rejects_a_foreign_handle_without_registering_its_ref() {
        let mut source = open_minimal();
        let foreign = source.get_object_handle(ObjectRef::new(99, 0));
        let mut destination = open_minimal();

        assert!(FileSpec::create_file_spec(&mut destination, b"foreign.bin", foreign).is_err());
        assert_eq!(
            EmbeddedFileStream::create_ef_stream(&mut destination, b"payload")
                .unwrap()
                .object_ref(),
            Some(ObjectRef::new(4, 0)),
            "rejecting a foreign factory input must not register its object number"
        );
    }

    #[test]
    fn create_filespec_accepts_a_direct_value_with_a_foreign_descendant() {
        let mut source = open_minimal();
        let foreign = source.get_object_handle(ObjectRef::new(99, 0));
        let direct = ObjectHandle::dictionary(vec![(b"Foreign".to_vec(), foreign)]);
        let mut destination = open_minimal();

        assert!(FileSpec::create_file_spec(&mut destination, b"direct.bin", direct).is_ok());
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Resolve the /EmbeddedFile stream dict for a filespec ref.
    fn resolve_ef_stream(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        fs_ref: ObjectRef,
    ) -> crate::object::Stream {
        let Some(fs_dict) = pdf
            .resolve_borrowed(fs_ref)
            .expect("resolve filespec")
            .as_dict()
        else {
            panic!("expected dictionary");
        };
        let ef_sub = match fs_dict.get("EF") {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => panic!("missing /EF"),
        };
        let stream_ref = match ef_sub.get("F") {
            Some(Object::Reference(r)) => *r,
            _ => panic!("missing /EF /F ref"),
        };
        match pdf.resolve_borrowed(stream_ref).expect("resolve stream") {
            Object::Stream(s) => s.clone(),
            _ => panic!("expected stream"),
        }
    }

    // ── Tests: FileSpecBuilder with compress(false) — existing behaviour ───────

    #[test]
    fn builder_uncompressed_round_trip() {
        let mut pdf = open_minimal();
        let raw = b"hello world";
        let fs_ref = FileSpecBuilder::new("test.txt", raw.as_ref())
            .build(&mut pdf)
            .expect("build");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        // No /Filter in uncompressed stream
        assert!(
            stream.dict.get("Filter").is_none(),
            "uncompressed stream must have no /Filter"
        );
        let decoded = decode_stream_data(&stream.dict, &stream.data).expect("decode");
        assert_eq!(decoded, raw);
    }

    #[test]
    fn builder_compressed_f_and_uf_follow_qpdf_unicode_string_rules() {
        let mut pdf = open_minimal();
        let raw = b"payload";
        let fs_ref = FileSpecBuilder::new("myfile.txt", raw.as_ref())
            .build(&mut pdf)
            .expect("build");

        let Some(fs_dict) = pdf
            .resolve_borrowed(fs_ref)
            .expect("resolve filespec")
            .as_dict()
        else {
            panic!("expected dictionary");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };
        assert_eq!(f, b"myfile.txt", "/F must be the filename");
        assert_eq!(uf, b"myfile.txt", "/UF must use qpdf newUnicodeString");
    }

    #[test]
    fn builder_allows_distinct_ascii_f_and_unicode_uf() {
        let mut pdf = open_minimal();
        let raw = b"payload";
        let fs_ref = FileSpecBuilder::new("____.pdf", raw.as_ref())
            .uf_filename("レポート.pdf")
            .build(&mut pdf)
            .expect("build");

        let Some(fs_dict) = pdf
            .resolve_borrowed(fs_ref)
            .expect("resolve filespec")
            .as_dict()
        else {
            panic!("expected dictionary");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };

        assert_eq!(f, b"____.pdf", "/F must be ASCII fallback");
        assert_eq!(
            uf,
            encode_utf16be("レポート.pdf"),
            "/UF must preserve the Unicode filename"
        );
    }

    // ── Tests: FileSpecBuilder → insert_embedded_file → list ─────────────────

    #[test]
    fn compressed_filespec_retrievable_via_list() {
        let mut pdf = open_minimal();
        let raw = b"retrievable payload";
        let fs_ref = FileSpecBuilder::new("list-test.txt", raw.as_ref())
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"list-test.txt", fs_ref).expect("insert");

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"list-test.txt");
        assert_eq!(entries[0].1, fs_ref);
    }

    #[test]
    fn existing_attachment_survives_second_insertion() {
        let mut pdf = open_minimal();

        // Insert first attachment (uncompressed for variety)
        let raw1 = b"first attachment";
        let fs1 = FileSpecBuilder::new("first.txt", raw1.as_ref())
            .build(&mut pdf)
            .expect("build first");
        insert_embedded_file(&mut pdf, b"first.txt", fs1).expect("insert first");

        // Insert second attachment (compressed)
        let raw2 = b"second attachment with more data";
        let fs2 = FileSpecBuilder::new("second.txt", raw2.as_ref())
            .build(&mut pdf)
            .expect("build second");
        insert_embedded_file(&mut pdf, b"second.txt", fs2).expect("insert second");

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 2, "both attachments must survive");
        let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(
            keys.contains(&b"first.txt".as_ref()),
            "first.txt must be present"
        );
        assert!(
            keys.contains(&b"second.txt".as_ref()),
            "second.txt must be present"
        );
    }

    // ── Tests: add_attachment_from_path ───────────────────────────────────────

    #[test]
    fn add_attachment_from_path_round_trip() {
        let mut pdf = open_minimal();

        // Write a temp file to attach.
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("hello.txt");
        let raw = b"Hello from disk!";
        std::fs::write(&file_path, raw).expect("write temp file");

        let fs_ref = add_attachment_from_path(&mut pdf, b"hello.txt", &file_path).expect("attach");

        // Verify retrievable via list_embedded_files
        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"hello.txt");
        assert_eq!(entries[0].1, fs_ref);

        // qpdf's addAttachments delegates to createFileSpec/createEFStream;
        // stream compression is selected later by the writer, not this helper.
        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        assert_eq!(
            stream.dict.get("Filter"),
            None,
            "attachment construction must not install a helper-local filter"
        );
        assert_eq!(stream.data, raw);
    }

    #[test]
    fn add_attachment_from_path_checksum_and_size() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("data.bin");
        let raw = b"deterministic checksum test data";
        std::fs::write(&file_path, raw).expect("write");

        let fs_ref = add_attachment_from_path(&mut pdf, b"data.bin", &file_path).expect("attach");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        let params = match stream.dict.get("Params") {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => panic!("missing /Params"),
        };
        let size = match params.get("Size") {
            Some(Object::Integer(n)) => *n,
            _ => panic!("missing /Params /Size"),
        };
        let checksum = match params.get("CheckSum") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /Params /CheckSum"),
        };
        assert_eq!(
            size,
            raw.len() as i64,
            "/Params /Size must match raw length"
        );
        assert_eq!(
            checksum,
            vec![
                0xcf, 0x5e, 0x73, 0xd1, 0x4d, 0xf5, 0xca, 0xd1, 0x94, 0xb0, 0x9e, 0xe5, 0x79, 0xf2,
                0x54, 0x9d,
            ],
            "/Params /CheckSum must be the MD5 of raw bytes"
        );
    }

    #[test]
    fn add_attachment_from_path_f_and_uf_follow_qpdf_unicode_string_rules() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("report.pdf");
        std::fs::write(&file_path, b"fake pdf content").expect("write");

        let fs_ref = add_attachment_from_path(&mut pdf, b"report.pdf", &file_path).expect("attach");

        let Some(fs_dict) = pdf.resolve_borrowed(fs_ref).expect("resolve").as_dict() else {
            panic!("expected dict");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };
        assert_eq!(f, b"report.pdf", "/F must be basename");
        assert_eq!(uf, b"report.pdf", "/UF must use qpdf's PDFDocEncoding form");
    }

    #[test]
    fn add_attachment_from_path_errors_on_missing_file() {
        let mut pdf = open_minimal();
        let result =
            add_attachment_from_path(&mut pdf, b"missing.txt", "/this/does/not/exist/missing.txt");
        assert!(result.is_err(), "must error when file does not exist");
        // Should be an Io error
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::Error::Io(_)),
            "expected Io error, got: {err:?}"
        );
    }

    #[test]
    fn add_attachment_from_path_accepts_non_ascii_basename() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("レポート.pdf");
        std::fs::write(&file_path, b"payload").expect("write temp file");

        let fs_ref = add_attachment_from_path(&mut pdf, "レポート.pdf".as_bytes(), &file_path)
            .expect("attach non-ASCII basename");

        let Some(fs_dict) = pdf.resolve_borrowed(fs_ref).expect("resolve").as_dict() else {
            panic!("expected dict");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };

        assert_eq!(f, b"____.pdf", "/F must be ASCII-safe fallback");
        assert_eq!(
            uf,
            encode_utf16be("レポート.pdf"),
            "/UF must preserve the Unicode basename"
        );
    }

    // ── Tests: extract_attachment / write_attachment / extract_attachment_to_path ─

    #[test]
    fn extract_attachment_small_round_trip() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.txt");
        let raw = b"Hello, world!";
        std::fs::write(&path, raw).expect("write");
        add_attachment_from_path(&mut pdf, b"small.txt", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"small.txt").expect("extract");
        assert_eq!(
            extracted.as_slice(),
            raw.as_ref(),
            "small file round-trip must match"
        );
    }

    #[test]
    fn extract_attachment_large_round_trip() {
        // 128 KiB of repeating pseudo-random-ish bytes — exercises compressor splits.
        let raw: Vec<u8> = (0u8..=255).cycle().take(128 * 1024).collect();
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("large.bin");
        std::fs::write(&path, &raw).expect("write");
        add_attachment_from_path(&mut pdf, b"large.bin", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"large.bin").expect("extract");
        assert_eq!(extracted, raw, "large file round-trip must match");
    }

    #[test]
    fn extract_attachment_binary_with_nuls_round_trip() {
        // 4096 bytes including NUL bytes, exercises binary safety.
        let raw: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("binary.bin");
        std::fs::write(&path, &raw).expect("write");
        add_attachment_from_path(&mut pdf, b"binary.bin", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"binary.bin").expect("extract");
        assert_eq!(extracted, raw, "binary file round-trip must match");
    }

    #[test]
    fn write_attachment_to_vec_matches_extract() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vec-test.txt");
        let raw = b"write_attachment test payload";
        std::fs::write(&path, raw).expect("write");
        add_attachment_from_path(&mut pdf, b"vec-test.txt", &path).expect("attach");

        let mut buf = Vec::new();
        write_attachment(&mut pdf, b"vec-test.txt", &mut buf).expect("write_attachment");
        assert_eq!(
            buf.as_slice(),
            raw.as_ref(),
            "write_attachment output must match raw"
        );
    }

    #[test]
    fn extract_attachment_to_path_round_trip() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");

        let src_path = dir.path().join("source.bin");
        let raw: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        std::fs::write(&src_path, &raw).expect("write source");
        add_attachment_from_path(&mut pdf, b"source.bin", &src_path).expect("attach");

        let out_path = dir.path().join("extracted.bin");
        extract_attachment_to_path(&mut pdf, b"source.bin", &out_path)
            .expect("extract_attachment_to_path");

        let read_back = std::fs::read(&out_path).expect("read back");
        assert_eq!(read_back, raw, "extract_to_path round-trip must match");
    }

    #[test]
    fn extract_attachment_missing_key_is_actionable_error() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("real.txt");
        std::fs::write(&path, b"real content").expect("write");
        add_attachment_from_path(&mut pdf, b"real.txt", &path).expect("attach");

        let err =
            extract_attachment(&mut pdf, b"missing-key").expect_err("must error for absent key");
        let msg = err.to_string();
        assert!(
            msg.contains("missing-key"),
            "error message must contain the missing key name, got: {msg}"
        );
        // Available keys hint must be present
        assert!(
            msg.contains("real.txt"),
            "error message must list available keys, got: {msg}"
        );
    }

    #[test]
    fn extract_attachment_from_compat_fixture() {
        // attachment-two-page.pdf contains an attachment under the key "attachment.txt"
        // with an uncompressed size of 95 bytes (from /Params /Size).
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/compat/attachment-two-page.pdf"
        );
        // The compat fixture is committed to the repo, so a missing file is a
        // real regression — fail loudly instead of silently skipping, which
        // could turn this into a false-positive pass (CodeRabbit).
        let file = std::fs::File::open(path)
            .expect("compat fixture missing: tests/fixtures/compat/attachment-two-page.pdf");
        let mut pdf = crate::Pdf::open(std::io::BufReader::new(file)).expect("open compat fixture");

        let entries = crate::embedded_files::list_embedded_files(&mut pdf).expect("list");
        assert!(
            !entries.is_empty(),
            "fixture must have at least one attachment"
        );

        // Use the first available key.
        let key = entries[0].0.clone();
        let extracted = extract_attachment(&mut pdf, &key).expect("extract from compat fixture");
        assert!(!extracted.is_empty(), "extracted bytes must be non-empty");

        // The fixture reports /Params /Size 95 — the extracted bytes must match.
        let mut fs = FileSpec::new(pdf.get_object_handle(entries[0].1), &mut pdf).unwrap();
        let ef = fs
            .embedded_file()
            .expect("embedded_file")
            .expect("must have embedded file");
        let reported_size = ef.size().expect("size").expect("size must be present");
        assert_eq!(
            extracted.len() as i64,
            reported_size,
            "extracted length must equal /Params /Size"
        );
    }
}

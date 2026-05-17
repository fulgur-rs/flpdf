//! Typed wrappers for `/Filespec` dictionaries and `/EmbeddedFile` streams.
//!
//! [`FileSpec`] wraps a `/Filespec` dictionary and exposes ergonomic, typed
//! accessors for all common fields (filename, description, embedded file
//! stream, etc.).  [`EmbeddedFileStream`] wraps the embedded `/EmbeddedFile`
//! stream reachable via the `/EF` sub-dictionary and exposes its payload and
//! metadata (MIME type, dates, checksum, size).
//!
//! Both types are **read-only**. [`FileSpec`] is a thin borrowing wrapper that
//! holds only the `/Filespec` `ObjectRef` and re-resolves the dictionary from
//! the live document on each accessor call. [`EmbeddedFileStream`] is
//! constructed once from an already-resolved `/EmbeddedFile` stream: it owns
//! that [`Stream`] and the `/Params` sub-dictionary resolved at construction
//! time (an indirect `/Params` is dereferenced once), so its metadata
//! accessors read this retained state rather than re-resolving.
//!
//! # Design
//!
//! PDF key naming follows ISO 32000-1 §7.11.  The `/EF` lookup priority used
//! here mirrors the qpdf JSON v2 `preferredcontents` order:
//! `/UF` › `/F` › `/Unix` › `/Mac` › `/DOS`.
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
//! let mut fs = FileSpec::new(filespec_ref, &mut pdf);
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
//! let mut fs = FileSpec::new(filespec_ref, &mut pdf);
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
use crate::object::{Dictionary, Object, Stream};
use crate::{Error, ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

// ── EmbeddedFileStream ────────────────────────────────────────────────────────

/// Wrapper for a `/EmbeddedFile` stream (ISO 32000-1 §7.11.4).
///
/// Construct via [`FileSpec::embedded_file`] rather than directly.
///
/// All accessors are cheap: only [`payload`](EmbeddedFileStream::payload)
/// performs I/O (decoding the filter chain).
pub struct EmbeddedFileStream<'a, R: Read + Seek> {
    /// The resolved `/EmbeddedFile` stream.  Stored by value because `Stream`
    /// owns its data, and we need the dict reference to survive across calls.
    stream: Stream,
    /// The `/Params` sub-dictionary, resolved at construction time so that
    /// metadata accessors stay `&self`.  `/Params` may be given as an
    /// indirect reference, which is dereferenced here.
    params: Option<Dictionary>,
    /// Kept to hold the document borrow for the wrapper's lifetime,
    /// mirroring [`FileSpec`]'s exclusive-borrow semantics.
    #[allow(dead_code)]
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> EmbeddedFileStream<'a, R> {
    fn new(stream: Stream, pdf: &'a mut Pdf<R>) -> Result<Self> {
        let params = match stream.dict.get("Params") {
            Some(Object::Dictionary(d)) => Some(d.clone()),
            Some(Object::Reference(r)) => {
                let r = *r;
                match pdf.resolve(r)? {
                    Object::Dictionary(d) => Some(d),
                    _ => None,
                }
            }
            _ => None,
        };
        Ok(Self {
            stream,
            params,
            pdf,
        })
    }

    /// Decode and return the raw payload bytes.
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
    /// # let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    /// if let Some(mut ef) = fs.embedded_file()? {
    ///     let data: Vec<u8> = ef.payload()?;
    ///     assert!(!data.is_empty());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn payload(&self) -> Result<Vec<u8>> {
        decode_stream_data(&self.stream.dict, &self.stream.data)
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
        Ok(match self.stream.dict.get("Subtype") {
            Some(Object::Name(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// Return the `/Params` sub-dictionary, if present.
    ///
    /// Resolved at construction time, so an indirect `/Params` reference is
    /// already dereferenced here.
    fn params(&self) -> Option<&Dictionary> {
        self.params.as_ref()
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
        Ok(self.params().and_then(|p| match p.get("CreationDate") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        }))
    }

    /// Return `/Params /ModDate` as a raw PDF date byte sequence.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn modification_date(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.params().and_then(|p| match p.get("ModDate") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        }))
    }

    /// Return `/Params /CheckSum` as raw bytes (typically a 16-byte MD5 hash).
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn checksum(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.params().and_then(|p| match p.get("CheckSum") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        }))
    }

    /// Return `/Params /Size` — the uncompressed file size in bytes.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn size(&self) -> Result<Option<i64>> {
        Ok(self.params().and_then(|p| match p.get("Size") {
            Some(Object::Integer(n)) => Some(*n),
            _ => None,
        }))
    }
}

// ── FileSpec ──────────────────────────────────────────────────────────────────

/// Wrapper for a `/Filespec` dictionary (ISO 32000-1 §7.11.3).
///
/// Construct with [`FileSpec::new`], passing the [`ObjectRef`] of a
/// `/Filespec` dictionary and a mutable borrow of the open document.
///
/// All accessors except [`embedded_file`](FileSpec::embedded_file) are
/// cheap dictionary lookups that return `Ok(None)` when the key is absent.
/// [`embedded_file`] resolves the `/EF /F` (or `/EF /UF`) indirect reference.
pub struct FileSpec<'a, R: Read + Seek> {
    filespec_ref: ObjectRef,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> FileSpec<'a, R> {
    /// Construct a new wrapper for the `/Filespec` dictionary at `filespec_ref`.
    ///
    /// The constructor does **not** resolve the reference — call individual
    /// accessors to fetch specific fields.
    pub fn new(filespec_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        Self { filespec_ref, pdf }
    }

    /// Resolve the `/Filespec` dictionary, returning an error when the
    /// object does not exist or is not a dictionary.
    fn resolve_dict(&mut self) -> Result<Dictionary> {
        match self.pdf.resolve(self.filespec_ref)? {
            Object::Dictionary(d) => Ok(d),
            _ => Err(Error::Unsupported(format!(
                "expected /Filespec dictionary at {}, got a non-dictionary object",
                self.filespec_ref
            ))),
        }
    }

    /// Return `/F` — the file name as raw PDF string bytes.
    ///
    /// Returns `None` when the key is absent or the value is not a string.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn filename(&mut self) -> Result<Option<Vec<u8>>> {
        let dict = self.resolve_dict()?;
        Ok(match dict.get("F") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
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
        let dict = self.resolve_dict()?;
        Ok(match dict.get("UF") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// Return `/Desc` — the file description as raw PDF string bytes.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn description(&mut self) -> Result<Option<Vec<u8>>> {
        let dict = self.resolve_dict()?;
        Ok(match dict.get("Desc") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// Return `/AFRelationship` — the associated-file relationship as raw
    /// PDF name bytes (e.g. `b"Source"`, `b"Data"`, `b"Alternative"`).
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn af_relationship(&mut self) -> Result<Option<Vec<u8>>> {
        let dict = self.resolve_dict()?;
        Ok(match dict.get("AFRelationship") {
            Some(Object::Name(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// Resolve and return the embedded file stream.
    ///
    /// The lookup priority for the `/EF` sub-dictionary key is
    /// `/UF`, `/F`, `/Unix`, `/Mac`, `/DOS` — the same preference order
    /// qpdf applies (Unicode name first), consistent with ISO 32000-1
    /// §7.11.4. The first key that resolves to an `/EmbeddedFile` stream
    /// reference is used.
    ///
    /// Returns `Ok(None)` when the `/Filespec` dictionary has no `/EF` entry
    /// or when none of the standard keys (`/UF`, `/F`, `/Unix`, `/Mac`,
    /// `/DOS`) resolve to an `/EmbeddedFile` stream.
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
    /// let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    /// if let Some(mut ef) = fs.embedded_file()? {
    ///     let bytes = ef.payload()?;
    ///     println!("{} bytes", bytes.len());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn embedded_file(&mut self) -> Result<Option<EmbeddedFileStream<'_, R>>> {
        let dict = self.resolve_dict()?;

        // Resolve /EF sub-dictionary.
        let ef_dict: Dictionary = match dict.get("EF") {
            Some(Object::Dictionary(d)) => d.clone(),
            Some(Object::Reference(r)) => {
                let r = *r;
                match self.pdf.resolve(r)? {
                    Object::Dictionary(d) => d,
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        // qpdf preference order: Unicode name first, then platform-specific.
        // Try each key in order and skip any that does not resolve to an
        // /EmbeddedFile stream, so a stray non-stream entry on a
        // higher-priority key does not mask a valid lower-priority one.
        let candidates: Vec<ObjectRef> = ["UF", "F", "Unix", "Mac", "DOS"]
            .iter()
            .filter_map(|k| match ef_dict.get(k) {
                Some(Object::Reference(r)) => Some(*r),
                _ => None,
            })
            .collect();

        for ef_ref in candidates {
            if let Object::Stream(stream) = self.pdf.resolve(ef_ref)? {
                return EmbeddedFileStream::new(stream, self.pdf).map(Some);
            }
        }

        Ok(None)
    }
}

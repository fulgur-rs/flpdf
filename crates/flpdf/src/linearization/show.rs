//! qpdf correspondence: QPDF_linearization.cc hint decoding plus QPDFJob.cc display formatting.
//! Linearization hint-stream decoder and `show-linearization` formatter.
//!
//! This module is the read-side inverse of the hint-stream encoder in
//! [`super::hint_stream`].  It decodes the FlateDecode-compressed hint stream of
//! a linearized PDF and renders a textual dump that reproduces qpdf's
//! `--show-linearization` output byte-for-byte (qpdf 11.9.0,
//! `QPDF::dumpLinearizationDataInternal`).
//!
//! # What is decoded
//!
//! * **Page Offset Hint Table** (ISO 32000-1 Annex F.3.1): a 13-field header
//!   followed by seven bit-packed columns (one byte-aligned after each), read
//!   exactly as qpdf's `readHPageOffset`.
//! * **Shared Object Hint Table** (Annex F.3.2): a 7-field header followed by
//!   four columns, read as qpdf's `readHSharedObject`.  There is no separate
//!   "groups" column in the read path; per-group object counts are carried by
//!   the `nobjects_minus_one` column.
//! * **Outlines Hint Table** (Annex F.3.4): the four-field generic table read by
//!   qpdf's `readHGeneric`, located only when the hint-stream dictionary has an
//!   `/O` key.  No fixture in this repository exercises it, so it is covered by
//!   unit tests.
//!
//! # Offsets
//!
//! Every byte offset printed (`first_page_offset`, `first_shared_offset`,
//! `first_object_offset`) is adjusted by qpdf's rule: the hint table locations
//! disregard the hint stream itself, so any raw offset `>= H_offset` has
//! `H_length` added before display (qpdf's `adjusted_offset`).

use super::check::{
    check_linearization_parameters, check_linearization_warnings, load_hint_stream_with_damage,
    HintStreamLoadError, LinearizationCheckError, LinearizationParameterCheck,
};
use crate::bit_stream::{BitStream, BitStreamError};
#[cfg(test)]
use crate::ObjectRef;
use crate::{ObjectHandle, Pdf};
use std::fmt;
use std::io::Cursor;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Reason a `show-linearization` decode failed.
///
/// Note that a PDF which is simply *not linearized* is **not** an error: qpdf's
/// `--show-linearization` prints `"<name> is not linearized"` to stdout and
/// exits 0 in that case, so [`show_linearization_bytes`] returns that line as an
/// `Ok` value rather than an error.
///
/// The public entry points in this module — [`show_linearization_bytes`],
/// [`show_linearization_bytes_with_warnings`], [`show_linearization_path`],
/// and [`show_linearization_path_with_warnings`] — never return
/// [`Malformed`](ShowLinearizationError::Malformed): mirroring qpdf's
/// `showLinearizationData`, which catches every parameter-dictionary or
/// hint-stream decode failure in one try/catch and reports it as a single
/// warning with no dump instead of raising a hard error, that class of
/// failure surfaces as an `Ok` value whose `dump` is empty and whose
/// `warnings` carries the one message.
#[derive(Debug)]
pub enum ShowLinearizationError {
    /// The linearization parameter dictionary or hint stream is malformed (a
    /// required key is missing or has the wrong type, an offset is out of
    /// bounds, or the bit stream is truncated). `message` describes the
    /// fault. See the enum-level documentation above: none of this module's
    /// public functions actually return this variant.
    Malformed { message: String },
    /// An I/O or parse error occurred while reading the file.
    Io(Box<dyn std::error::Error + Send + Sync>),
}

/// Successful `--show-linearization` output plus qpdf soft-check warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowLinearizationOutput {
    /// The qpdf-compatible dump written to stdout.
    pub dump: String,
    /// Ordered warning messages emitted by qpdf's linearization checker.
    pub warnings: Vec<String>,
}

impl fmt::Display for ShowLinearizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShowLinearizationError::Malformed { message } => {
                write!(f, "malformed linearization data: {message}")
            }
            ShowLinearizationError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ShowLinearizationError {}

impl From<crate::Error> for ShowLinearizationError {
    fn from(e: crate::Error) -> Self {
        ShowLinearizationError::Io(Box::new(e))
    }
}

impl From<std::io::Error> for ShowLinearizationError {
    fn from(e: std::io::Error) -> Self {
        ShowLinearizationError::Io(Box::new(e))
    }
}

impl From<BitStreamError> for ShowLinearizationError {
    fn from(error: BitStreamError) -> Self {
        ShowLinearizationError::Malformed {
            message: error.to_string(),
        }
    }
}

/// Shorthand result type for the decoder.
pub(crate) type ShowResult<T> = std::result::Result<T, ShowLinearizationError>;

/// Return a [`ShowLinearizationError::Malformed`] error with a formatted message.
macro_rules! malformed {
    ($($arg:tt)*) => {
        ShowLinearizationError::Malformed { message: format!($($arg)*) }
    };
}

enum ShowTablesError {
    QpdfDamage {
        object: &'static str,
        offset: u64,
        detail: String,
    },
    Other(ShowLinearizationError),
}

impl From<ShowLinearizationError> for ShowTablesError {
    fn from(error: ShowLinearizationError) -> Self {
        Self::Other(error)
    }
}

fn load_hint_stream_for_show(
    pdf: &mut Pdf<Cursor<Vec<u8>>>,
    file_bytes: &[u8],
    offset: usize,
    expected_h_length: u64,
) -> std::result::Result<(ObjectHandle, Rc<Vec<u8>>), ShowTablesError> {
    match load_hint_stream_with_damage(pdf, file_bytes, offset, expected_h_length) {
        Ok(result) => Ok(result),
        Err(HintStreamLoadError::Damage(damage)) => Err(ShowTablesError::QpdfDamage {
            object: damage.object,
            offset: damage.offset,
            detail: damage.detail,
        }),
        Err(HintStreamLoadError::Core(crate::Error::Unsupported(message))) => {
            Err(malformed!("{message}").into())
        }
        Err(HintStreamLoadError::Core(error)) => Err(ShowLinearizationError::from(error).into()), // cov:ignore: Cursor<Vec<u8>> show inputs cannot produce this generic source-I/O arm
    }
}

// ---------------------------------------------------------------------------
// Decoded data — qpdf-shaped structs (map 1:1 to the dump format)
// ---------------------------------------------------------------------------

/// Linearization parameter-dictionary values (qpdf's `LinParameters`).
struct LinParameters {
    file_size: u64,
    first_page_object: u64,
    first_page_end: u64,
    npages: u32,
    xref_zero_offset: u64,
    first_page: i64,
    h_offset: u64,
    h_length: u64,
    h_overflow_offset: u64,
    h_overflow_length: u64,
}

impl LinParameters {
    /// qpdf's `adjusted_offset`: hint table locations disregard the hint stream
    /// itself, so any raw offset `>= H_offset` is increased by `H_length`.
    ///
    /// `wrapping_add` matches qpdf's C++ unsigned wraparound and avoids Rust's
    /// debug-build overflow panic on a malformed offset/length; valid hint
    /// tables never approach `u64::MAX`, so the displayed value is unchanged.
    fn adjusted_offset(&self, offset: u64) -> u64 {
        if offset >= self.h_offset {
            offset.wrapping_add(self.h_length)
        } else {
            offset
        }
    }
}

/// Decoded Page Offset Hint Table (qpdf's `HPageOffset`).
#[derive(Clone)]
pub(crate) struct HPageOffset {
    pub(crate) min_nobjects: u32,
    pub(crate) first_page_offset: u64,
    pub(crate) nbits_delta_nobjects: u32,
    pub(crate) min_page_length: u64,
    pub(crate) nbits_delta_page_length: u32,
    pub(crate) min_content_offset: u64,
    pub(crate) nbits_delta_content_offset: u32,
    pub(crate) min_content_length: u64,
    pub(crate) nbits_delta_content_length: u32,
    pub(crate) nbits_nshared_objects: u32,
    pub(crate) nbits_shared_identifier: u32,
    pub(crate) nbits_shared_numerator: u32,
    pub(crate) shared_denominator: u32,
    pub(crate) entries: Vec<HPageOffsetEntry>,
}

/// Decoded per-page entry of the Page Offset Hint Table.
#[derive(Clone)]
pub(crate) struct HPageOffsetEntry {
    pub(crate) delta_nobjects: u64,
    pub(crate) delta_page_length: u64,
    pub(crate) nshared_objects: u32,
    pub(crate) shared_identifiers: Vec<u64>,
    pub(crate) shared_numerators: Vec<u64>,
    pub(crate) delta_content_offset: u64,
    pub(crate) delta_content_length: u64,
}

/// Decoded Shared Object Hint Table (qpdf's `HSharedObject`).
#[derive(Clone)]
pub(crate) struct HSharedObject {
    pub(crate) first_shared_obj: u64,
    pub(crate) first_shared_offset: u64,
    pub(crate) nshared_first_page: u32,
    pub(crate) nshared_total: u32,
    pub(crate) nbits_nobjects: u32,
    pub(crate) min_group_length: u64,
    pub(crate) nbits_delta_group_length: u32,
    pub(crate) entries: Vec<HSharedObjectEntry>,
}

/// Decoded per-entry data of the Shared Object Hint Table.
#[derive(Clone)]
pub(crate) struct HSharedObjectEntry {
    pub(crate) delta_group_length: u64,
    pub(crate) signature_present: bool,
    pub(crate) nobjects_minus_one: u64,
}

/// Decoded generic (Outlines) hint table (qpdf's `HGeneric`).
#[derive(Clone)]
pub(crate) struct HGeneric {
    pub(crate) first_object: u64,
    pub(crate) first_object_offset: u64,
    pub(crate) nobjects: u32,
    pub(crate) group_length: u64,
}

// ---------------------------------------------------------------------------
// Page Offset Hint Table decoder — mirrors qpdf readHPageOffset
// ---------------------------------------------------------------------------

pub(crate) fn read_h_page_offset(buf: &[u8], npages: u32) -> ShowResult<HPageOffset> {
    let mut bits = BitStream::new(buf);

    // 13 header fields (5×32 + 8×16 = 288 bits = 36 bytes, leaving the reader
    // byte-aligned).  Order is qpdf's readHPageOffset.
    let min_nobjects = bits.get_bits_i32(32)? as u32;
    let first_page_offset = bits.get_bits_i32(32)? as u32 as u64;
    let nbits_delta_nobjects = bits.get_bits_i32(16)? as u32;
    let min_page_length = bits.get_bits_i32(32)? as u32 as u64;
    let nbits_delta_page_length = bits.get_bits_i32(16)? as u32;
    let min_content_offset = bits.get_bits_i32(32)? as u32 as u64;
    let nbits_delta_content_offset = bits.get_bits_i32(16)? as u32;
    let min_content_length = bits.get_bits_i32(32)? as u32 as u64;
    let nbits_delta_content_length = bits.get_bits_i32(16)? as u32;
    let nbits_nshared_objects = bits.get_bits_i32(16)? as u32;
    let nbits_shared_identifier = bits.get_bits_i32(16)? as u32;
    let nbits_shared_numerator = bits.get_bits_i32(16)? as u32;
    let shared_denominator = bits.get_bits_i32(16)? as u32;

    let n = npages as usize;
    let mut entries: Vec<HPageOffsetEntry> = (0..n)
        .map(|_| HPageOffsetEntry {
            delta_nobjects: 0,
            delta_page_length: 0,
            nshared_objects: 0,
            shared_identifiers: Vec::new(),
            shared_numerators: Vec::new(),
            delta_content_offset: 0,
            delta_content_length: 0,
        })
        .collect();

    // (a) delta_nobjects
    for e in entries.iter_mut() {
        e.delta_nobjects = bits.get_bits_i32(nbits_delta_nobjects as usize)? as u32 as u64;
    }
    bits.skip_to_next_byte()?;
    // (b) delta_page_length
    for e in entries.iter_mut() {
        e.delta_page_length = bits.get_bits_i32(nbits_delta_page_length as usize)? as u32 as u64;
    }
    bits.skip_to_next_byte()?;
    // (c) nshared_objects
    for e in entries.iter_mut() {
        e.nshared_objects = bits.get_bits_i32(nbits_nshared_objects as usize)? as u32;
    }
    bits.skip_to_next_byte()?;
    // Bound the per-page shared-object refs before the nested (d)/(e) loops.
    // With zero-width identifier/numerator fields each push reads 0 bits and
    // never advances the reader, so an untrusted `nshared_objects` could
    // otherwise drive unbounded time/allocation from a tiny stream. Cap the
    // total against the bits remaining, keeping work O(stream length).
    let remaining_bits = bits.bits_available();
    let total_shared: u64 = entries.iter().map(|e| e.nshared_objects as u64).sum();
    if total_shared > remaining_bits as u64 {
        return Err(malformed!(
            "page offset hint table claims {total_shared} shared-object refs but only {remaining_bits} bits remain"
        ));
    }
    // (d) shared_identifiers — nested: per page, read that page's count
    for e in entries.iter_mut() {
        for _ in 0..e.nshared_objects {
            e.shared_identifiers
                .push(bits.get_bits_i32(nbits_shared_identifier as usize)? as u32 as u64);
        }
    }
    bits.skip_to_next_byte()?;
    // (e) shared_numerators — nested
    for e in entries.iter_mut() {
        for _ in 0..e.nshared_objects {
            e.shared_numerators
                .push(bits.get_bits_i32(nbits_shared_numerator as usize)? as u32 as u64);
        }
    }
    bits.skip_to_next_byte()?;
    // (f) delta_content_offset
    for e in entries.iter_mut() {
        e.delta_content_offset =
            bits.get_bits_i32(nbits_delta_content_offset as usize)? as u32 as u64;
    }
    bits.skip_to_next_byte()?;
    // (g) delta_content_length
    for e in entries.iter_mut() {
        e.delta_content_length =
            bits.get_bits_i32(nbits_delta_content_length as usize)? as u32 as u64;
    }
    bits.skip_to_next_byte()?;

    Ok(HPageOffset {
        min_nobjects,
        first_page_offset,
        nbits_delta_nobjects,
        min_page_length,
        nbits_delta_page_length,
        min_content_offset,
        nbits_delta_content_offset,
        min_content_length,
        nbits_delta_content_length,
        nbits_nshared_objects,
        nbits_shared_identifier,
        nbits_shared_numerator,
        shared_denominator,
        entries,
    })
}

// ---------------------------------------------------------------------------
// Shared Object Hint Table decoder — mirrors qpdf readHSharedObject
// ---------------------------------------------------------------------------

pub(crate) fn read_h_shared_object(buf: &[u8]) -> ShowResult<HSharedObject> {
    let mut bits = BitStream::new(buf);

    // 7 header fields.
    let first_shared_obj = bits.get_bits_i32(32)? as u32 as u64;
    let first_shared_offset = bits.get_bits_i32(32)? as u32 as u64;
    let nshared_first_page = bits.get_bits_i32(32)? as u32;
    let nshared_total = bits.get_bits_i32(32)? as u32;
    let nbits_nobjects = bits.get_bits_i32(16)? as u32;
    let min_group_length = bits.get_bits_i32(32)? as u32 as u64;
    let nbits_delta_group_length = bits.get_bits_i32(16)? as u32;

    // Each entry consumes at least one bit (the signature_present column below),
    // so a well-formed table cannot claim more entries than there are bits left
    // in the stream. Guard before allocating so a malformed `nshared_total`
    // (up to u32::MAX) cannot drive a multi-gigabyte pre-allocation (OOM DoS).
    let remaining_bits = bits.bits_available();
    if nshared_total as usize > remaining_bits {
        return Err(malformed!(
            "shared-object hint table claims {nshared_total} entries but only {remaining_bits} bits remain"
        ));
    }

    let n = nshared_total as usize;
    let mut entries: Vec<HSharedObjectEntry> = (0..n)
        .map(|_| HSharedObjectEntry {
            delta_group_length: 0,
            signature_present: false,
            nobjects_minus_one: 0,
        })
        .collect();

    // (a) delta_group_length
    for e in entries.iter_mut() {
        e.delta_group_length = bits.get_bits_i32(nbits_delta_group_length as usize)? as u32 as u64;
    }
    bits.skip_to_next_byte()?;
    // (b) signature_present (1 bit each)
    for e in entries.iter_mut() {
        e.signature_present = bits.get_bits_i32(1)? != 0;
    }
    bits.skip_to_next_byte()?;
    // (c) inline 128-bit signature skip per set flag (no alignment around it),
    // matching qpdf's loop between the signature_present and nobjects columns.
    for e in entries.iter() {
        if e.signature_present {
            for _ in 0..4 {
                let _ = bits.get_bits(32)?;
            }
        }
    }
    // (d) nobjects_minus_one
    for e in entries.iter_mut() {
        e.nobjects_minus_one = bits.get_bits_i32(nbits_nobjects as usize)? as u32 as u64;
    }
    bits.skip_to_next_byte()?;

    Ok(HSharedObject {
        first_shared_obj,
        first_shared_offset,
        nshared_first_page,
        nshared_total,
        nbits_nobjects,
        min_group_length,
        nbits_delta_group_length,
        entries,
    })
}

// ---------------------------------------------------------------------------
// Generic (Outlines) Hint Table decoder — mirrors qpdf readHGeneric
// ---------------------------------------------------------------------------

pub(crate) fn read_h_generic(buf: &[u8]) -> ShowResult<HGeneric> {
    let mut bits = BitStream::new(buf);
    let first_object = bits.get_bits_i32(32)? as u32 as u64;
    let first_object_offset = bits.get_bits_i32(32)? as u32 as u64;
    let nobjects = bits.get_bits_i32(32)? as u32;
    let group_length = bits.get_bits_i32(32)? as u32 as u64;
    Ok(HGeneric {
        first_object,
        first_object_offset,
        nobjects,
        group_length,
    })
}

// ---------------------------------------------------------------------------
// Formatting — byte-for-byte reproduction of qpdf dumpLinearization*
// ---------------------------------------------------------------------------

fn dump_page_offset(out: &mut String, p: &LinParameters, t: &HPageOffset) {
    use std::fmt::Write;
    let _ = write!(
        out,
        "min_nobjects: {}\n\
         first_page_offset: {}\n\
         nbits_delta_nobjects: {}\n\
         min_page_length: {}\n\
         nbits_delta_page_length: {}\n\
         min_content_offset: {}\n\
         nbits_delta_content_offset: {}\n\
         min_content_length: {}\n\
         nbits_delta_content_length: {}\n\
         nbits_nshared_objects: {}\n\
         nbits_shared_identifier: {}\n\
         nbits_shared_numerator: {}\n\
         shared_denominator: {}\n",
        t.min_nobjects,
        p.adjusted_offset(t.first_page_offset),
        t.nbits_delta_nobjects,
        t.min_page_length,
        t.nbits_delta_page_length,
        t.min_content_offset,
        t.nbits_delta_content_offset,
        t.min_content_length,
        t.nbits_delta_content_length,
        t.nbits_nshared_objects,
        t.nbits_shared_identifier,
        t.nbits_shared_numerator,
        t.shared_denominator,
    );

    for (i, pe) in t.entries.iter().enumerate() {
        let _ = write!(
            out,
            "Page {i}:\n  \
             nobjects: {}\n  \
             length: {}\n  \
             content_offset: {}\n  \
             content_length: {}\n  \
             nshared_objects: {}\n",
            // wrapping_add: avoid Rust's debug overflow panic on malformed
            // 64-bit deltas (matches qpdf's C++ unsigned wraparound); valid
            // tables never overflow, so the displayed values are unchanged.
            pe.delta_nobjects.wrapping_add(t.min_nobjects as u64),
            pe.delta_page_length.wrapping_add(t.min_page_length),
            pe.delta_content_offset.wrapping_add(t.min_content_offset),
            pe.delta_content_length.wrapping_add(t.min_content_length),
            pe.nshared_objects,
        );
        for j in 0..pe.nshared_objects as usize {
            let _ = writeln!(out, "    identifier {j}: {}", pe.shared_identifiers[j]);
            let _ = writeln!(out, "    numerator {j}: {}", pe.shared_numerators[j]);
        }
    }
}

fn dump_shared_object(out: &mut String, p: &LinParameters, t: &HSharedObject) {
    use std::fmt::Write;
    let _ = write!(
        out,
        "first_shared_obj: {}\n\
         first_shared_offset: {}\n\
         nshared_first_page: {}\n\
         nshared_total: {}\n\
         nbits_nobjects: {}\n\
         min_group_length: {}\n\
         nbits_delta_group_length: {}\n",
        t.first_shared_obj,
        p.adjusted_offset(t.first_shared_offset),
        t.nshared_first_page,
        t.nshared_total,
        t.nbits_nobjects,
        t.min_group_length,
        t.nbits_delta_group_length,
    );

    for (i, se) in t.entries.iter().enumerate() {
        let _ = write!(
            out,
            "Shared Object {i}:\n  group length: {}\n",
            se.delta_group_length.wrapping_add(t.min_group_length)
        );
        // qpdf prints these only when set / non-zero.
        if se.signature_present {
            out.push_str("  signature present\n");
        }
        if se.nobjects_minus_one != 0 {
            let _ = writeln!(out, "  nobjects: {}", se.nobjects_minus_one.wrapping_add(1));
        }
    }
}

fn dump_generic(out: &mut String, p: &LinParameters, t: &HGeneric) {
    use std::fmt::Write;
    let _ = write!(
        out,
        "first_object: {}\n\
         first_object_offset: {}\n\
         nobjects: {}\n\
         group_length: {}\n",
        t.first_object,
        p.adjusted_offset(t.first_object_offset),
        t.nobjects,
        t.group_length,
    );
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Read a required non-negative integer parameter from a canonical
/// linearization dictionary handle.
fn param_u64(dict: &ObjectHandle, key: &'static str) -> ShowResult<u64> {
    let value = dict
        .try_get_key(format!("/{key}").as_bytes())
        .map_err(ShowLinearizationError::from)?;
    integer_u64(&value, key)
}

/// Read a non-negative integer from an already-selected canonical value
/// handle. This is used for array items such as qpdf's `/H` fields, where a
/// second dictionary lookup would be a type error.
fn integer_u64(value: &ObjectHandle, key: &str) -> ShowResult<u64> {
    value
        .try_dereference()
        .map_err(ShowLinearizationError::from)?;
    match value.as_integer() {
        Some(n) if n >= 0 => Ok(n as u64),
        _ => Err(malformed!(
            "/{key} is missing or not a non-negative integer in the linearization dictionary"
        )),
    }
}

/// Apply qpdf's `isLinearized` test to a candidate first-object dictionary.
///
/// Returns `true` when `/Linearized` is a number whose floor is 1 and, when
/// `/L` is an integer, `/L` equals `file_size` (qpdf returns false on a `/L`
/// mismatch — i.e. treats the file as not linearized).
#[cfg(test)]
fn is_linearized(dict: &ObjectHandle, file_size: u64) -> ShowResult<bool> {
    dict.try_dereference()
        .map_err(ShowLinearizationError::from)?;
    let linearized = dict
        .try_get_key(b"/Linearized")
        .map_err(ShowLinearizationError::from)?;
    linearized
        .try_dereference()
        .map_err(ShowLinearizationError::from)?;
    let linearized_ok = linearized
        .as_integer()
        .map(|value| value as f64)
        .or_else(|| linearized.as_real())
        .is_some_and(|value| value.is_finite() && value.floor() == 1.0);
    if !linearized_ok {
        return Ok(false);
    }
    let length = dict
        .try_get_key(b"/L")
        .map_err(ShowLinearizationError::from)?;
    length
        .try_dereference()
        .map_err(ShowLinearizationError::from)?;
    if let Some(l) = length.as_integer() {
        if l < 0 || l as u64 != file_size {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Extract qpdf's `LinParameters` from the linearization parameter dictionary.
///
/// `file_size` is the actual file length (used directly; qpdf's `isLinearized`
/// has already verified that `/L` equals it).
fn read_lin_parameters(dict: &ObjectHandle, file_size: u64) -> ShowResult<LinParameters> {
    let first_page_object = param_u64(dict, "O")?;
    let first_page_end = param_u64(dict, "E")?;
    let npages_u64 = param_u64(dict, "N")?;
    let npages = u32::try_from(npages_u64)
        // cov:ignore: a document with > u32::MAX pages cannot be opened, so this
        // overflow guard is unreachable through the public API.
        .map_err(|_| malformed!("/N ({npages_u64}) does not fit in u32"))?;
    let xref_zero_offset = param_u64(dict, "T")?;
    // /P (first page number) is optional; qpdf defaults to 0 when absent/null.
    let p = dict
        .try_get_key(b"/P")
        .map_err(ShowLinearizationError::from)?;
    p.try_dereference().map_err(ShowLinearizationError::from)?;
    let first_page: i64 = if let Some(n) = p.as_integer() {
        n
    } else if p.is_null() {
        0
    } else {
        return Err(malformed!("/P is present but not an integer"));
    };
    // /H is [offset length] or [offset length offset length] for an overflow
    // table. qpdf rejects every other cardinality before reading the items.
    let h = dict
        .try_get_key(b"/H")
        .map_err(ShowLinearizationError::from)?;
    h.try_dereference().map_err(ShowLinearizationError::from)?;
    let Some(h_items) = h.as_array() else {
        return Err(malformed!("/H is missing or not an [offset length] array"));
    };
    if !matches!(h_items.len(), 2 | 4) {
        return Err(malformed!(
            "/H has the wrong number of items (expected 2 or 4, got {})",
            h_items.len()
        ));
    }
    let h_offset = integer_u64(&h_items[0], "H[0]")?;
    let h_length = integer_u64(&h_items[1], "H[1]")?;
    let (h_overflow_offset, h_overflow_length) = if h_items.len() == 4 {
        (
            integer_u64(&h_items[2], "H[2]")?,
            integer_u64(&h_items[3], "H[3]")?,
        )
    } else {
        (0, 0)
    };
    Ok(LinParameters {
        file_size,
        first_page_object,
        first_page_end,
        npages,
        xref_zero_offset,
        first_page,
        h_offset,
        h_length,
        h_overflow_offset,
        h_overflow_length,
    })
}

/// Format a qpdf `damagedPDF` warning raised while loading linearization data.
fn linearization_parameter_warning(
    pdf: &mut Pdf<Cursor<Vec<u8>>>,
    display_name: &str,
    message: &str,
) -> ShowResult<String> {
    for object in ["linearization dictionary", "linearization hint table"] {
        let prefix = format!("{object}: ");
        if let Some(detail) = message.strip_prefix(&prefix) {
            let offset = if object == "linearization dictionary" {
                let candidate = pdf
                    .linearization_candidate_ref()
                    .map_err(ShowLinearizationError::from)?;
                candidate
                    .map(|object_ref| pdf.get_object_handle(object_ref).get_parsed_offset())
                    .filter(|offset| *offset >= 0)
                    .map(|offset| offset as u64)
                    .unwrap_or_else(|| pdf.source_last_offset())
            } else {
                pdf.source_last_offset()
            };
            return Ok(format!(
                "{display_name} ({object}, offset {offset}): {detail}"
            ));
        }
    }
    // The checker supplies only the two qpdf damagedPDF categories above;
    // keep a stable fallback for future diagnostics.
    Ok(format!("{display_name}: {message}")) // cov:ignore: unreachable fallback for unknown qpdf category
}

/// Extract the Shared Object (`/S`) and optional Outline (`/O`) section offsets
/// from the **hint stream** dictionary (not the parameter dict).
///
/// Returns `(s_offset, outline_offset)`.
pub(crate) fn read_hint_offsets(hint_dict: &ObjectHandle) -> ShowResult<(usize, Option<usize>)> {
    let s = hint_dict
        .try_get_key(b"/S")
        .map_err(ShowLinearizationError::from)?;
    let s_value = integer_u64(&s, "S")?;
    let s_offset = usize::try_from(s_value)
        // cov:ignore: on 64-bit usize is u64, so a non-negative /S offset
        // always fits; this only fires on 32-bit targets.
        .map_err(|_| malformed!("hint stream /S offset does not fit in platform usize"))?;

    let outline = hint_dict
        .try_get_key(b"/O")
        .map_err(ShowLinearizationError::from)?;
    outline
        .try_dereference()
        .map_err(ShowLinearizationError::from)?;
    let outline_offset = if let Some(value) = outline.as_integer() {
        if value < 0 {
            return Err(malformed!("hint stream /O offset is negative"));
        }
        Some(
            usize::try_from(value as u64)
                // cov:ignore: on 64-bit usize is u64, so a non-negative /O
                // offset always fits; this only fires on 32-bit targets.
                .map_err(|_| malformed!("hint stream /O offset does not fit in platform usize"))?,
        )
    } else {
        None
    };
    Ok((s_offset, outline_offset))
}

/// Decode the linearization data of `pdf` and format it like qpdf
/// `--show-linearization`, using `display_name` for the leading filename line.
///
/// Monomorphic over `Cursor<Vec<u8>>` (both public wrappers open the bytes that
/// way) so coverage instrumentation attributes its body to a single instance.
fn show_with_pdf(
    pdf: &mut Pdf<Cursor<Vec<u8>>>,
    file_bytes: &[u8],
    display_name: &str,
) -> ShowResult<ShowLinearizationOutput> {
    let file_size = file_bytes.len() as u64;

    // 1. Locate the linearization parameter dictionary (first physical object)
    //    and apply qpdf's `isLinearized` test: the first object must be a dict
    //    with a numeric `/Linearized` whose floor is 1, and (when `/L` is an
    //    integer) `/L` must equal the file length.  When the file is not
    //    linearized, qpdf's `--show-linearization` prints "<name> is not
    //    linearized" to stdout and exits 0 — we return that line as Ok.
    let not_linearized = || {
        Ok(ShowLinearizationOutput {
            dump: format!("{display_name} is not linearized\n"),
            warnings: Vec::new(),
        })
    };

    if !pdf.is_linearized().map_err(ShowLinearizationError::from)? {
        return not_linearized();
    }
    let Some(first_obj_ref) = pdf
        .linearization_candidate_ref()
        .map_err(ShowLinearizationError::from)?
    else {
        return not_linearized();
    };
    let param_dict = pdf.get_object_handle(first_obj_ref);
    if param_dict
        .try_as_dictionary()
        .map_err(ShowLinearizationError::from)?
        .is_none()
    {
        return not_linearized();
    }

    // qpdf's `showLinearizationData` reads and decodes the hint tables first,
    // then calls `checkLinearizationInternal`, and always dumps after the
    // check (`QPDF_linearization.cc:837-846`). The canonical warning route
    // preserves that order; the CLI owns logger emission and exit-3 completion.
    let mut warnings = match check_linearization_parameters(pdf)? {
        LinearizationParameterCheck::Clean => Vec::new(),
        LinearizationParameterCheck::Warning(message) => vec![message.to_owned()],
        LinearizationParameterCheck::Error(message) => {
            return Ok(ShowLinearizationOutput {
                dump: String::new(),
                warnings: vec![linearization_parameter_warning(pdf, display_name, message)?],
            });
        }
    };

    // qpdf's showLinearizationData wraps readLinearizationData,
    // checkLinearizationInternal, and dumpLinearizationDataInternal in a single try/catch
    // (QPDF_linearization.cc:837-846): a QPDFExc thrown by either step
    // collapses to one bare linearizationWarning(e.what()) with no dump
    // (QPDF::linearizationWarning, `:63-67` — the message carries no
    // "error encountered while..." prefix; that prefix belongs to
    // checkLinearization's own catch, `:70-79`, used by the `--check` route
    // in job/check.rs instead). checkLinearizationInternal sits inside the
    // same try block but never throws — every mismatch it finds is a soft
    // warning that does not stop the dump (`:452-470` for /T specifically).
    // Reproduce that split here: a Malformed error from the hint-stream load
    // or hint-table decode replaces whatever warnings have accumulated so far
    // with that one message and skips the dump, but
    // check_linearization_warnings's InvalidParam appends after successful
    // readLinearizationData and lets the dump proceed.
    let tables = (|| -> std::result::Result<
        (LinParameters, HPageOffset, HSharedObject, Option<HGeneric>),
        ShowTablesError,
    > {
            // 2. Param-dict values (qpdf's LinParameters).
            let params = read_lin_parameters(&param_dict, file_size)?;
            // 3. Locate, resolve, and decompress the hint stream object at /H[0].
            //
            // An unreadable/out-of-bounds/undecodable hint stream is caught here,
            // matching qpdf's actual `readHintStream` throw site
            // (`QPDF_linearization.cc:284-322`). decode_failure_is_malformed
            // exercises this directly with a corrupted deflate payload.
            let h_usize = usize::try_from(params.h_offset)
                // cov:ignore: on 64-bit usize is u64, so a non-negative h_offset always
                // fits; this only fires on 32-bit targets.
                .map_err(|_| malformed!("/H[0] does not fit in platform usize"))?;
            let (hint_dict, primary_decompressed) =
                load_hint_stream_for_show(pdf, file_bytes, h_usize, params.h_length)?;
            // qpdf pipes the primary and (when present) overflow hint streams into
            // the SAME buffer before parsing any table (`readLinearizationData`,
            // `QPDF_linearization.cc:241-245`: both `readHintStream` calls write to
            // one shared `Pl_Buffer pb`). All /S and /O offsets below index into that
            // concatenation, not just the primary stream's bytes, so a genuine
            // four-item /H splitting real data across two streams must be merged the
            // same way rather than silently decoding only the primary half.
            let mut decompressed =
                Rc::try_unwrap(primary_decompressed).unwrap_or_else(|shared| (*shared).clone());

            if params.h_overflow_offset != 0 {
                let overflow_offset = usize::try_from(params.h_overflow_offset)
                    // cov:ignore: on 64-bit usize is u64, so a non-negative overflow
                    // offset always fits; this only fires on 32-bit targets.
                    .map_err(|_| malformed!("/H[2] does not fit in platform usize"))?;
                let (_overflow_dict, overflow_decompressed) = load_hint_stream_for_show(
                    pdf,
                    file_bytes,
                    overflow_offset,
                    params.h_overflow_length,
                )?;
                decompressed.extend_from_slice(&overflow_decompressed);
            }

            // /S (shared object table offset) and /O (outline table offset) are keys on
            // the HINT STREAM dictionary — not the parameter dict.
            let (s_offset, outline_offset) = read_hint_offsets(&hint_dict)?;
            if s_offset >= decompressed.len() {
                return Err(ShowTablesError::QpdfDamage {
                    object: "linearization hint table",
                    offset: pdf.source_last_offset(),
                    detail: "/S (shared object) offset is out of bounds".to_owned(),
                });
            }

            // 4. Decode each table from a fresh reader at its offset.
            let page_offset = read_h_page_offset(&decompressed, params.npages)?;
            let shared_object = read_h_shared_object(&decompressed[s_offset..])?;
            let outline = match outline_offset {
                // cov:ignore-start: Outlines hint table — flpdf never emits /O on the
                // hint dict and no fixture has one, so this arm is unreachable through
                // flpdf's output (read_h_generic itself is unit-tested directly).
                Some(off) => {
                    if off >= decompressed.len() {
                        return Err(ShowTablesError::QpdfDamage {
                            object: "linearization hint table",
                            offset: pdf.source_last_offset(),
                            detail: "/O (outline) offset is out of bounds".to_owned(),
                        });
                    }
                    Some(read_h_generic(&decompressed[off..])?)
                }
                // cov:ignore-end
                None => None,
            };

            // qpdf checks the already-read linearization data only after
            // `readLinearizationData` has completed (`QPDF_linearization.cc:
            // 837-846`). Keeping this after both hint streams have loaded is
            // also important for the qpdf damage offset: a failed first load
            // must not be retried after its object has entered the cache.
            match check_linearization_warnings(pdf, file_bytes, true) {
                Ok(messages) => warnings.extend(messages),
                // cov:ignore-start: a Cursor<Vec<u8>> cannot produce a source I/O error;
                // retain the defensive mapping for the generic checker contract.
                Err(LinearizationCheckError::Io(error)) => {
                    return Err(ShowLinearizationError::Io(error).into()); // cov:ignore: in-memory source failures are defensive
                }
                // cov:ignore-end
                // qpdf's successful readLinearizationData/checkLinearizationInternal
                // route emits only soft warnings. Keep this defensive mapping for
                // the stricter standalone checker contract; it has no qpdf
                // showLinearizationData counterpart.
                // cov:ignore-start: no qpdf-compatible show path returns InvalidParam after the
                // hint data has been read successfully; the arm is retained as a defensive
                // mapping for the shared standalone-checker contract.
                Err(LinearizationCheckError::InvalidParam { message }) => {
                    warnings.push(message);
                }
                // cov:ignore-end
                // cov:ignore-start: show_with_pdf already confirmed
                // pdf.is_linearized() above, so check_linearization_warnings
                // cannot reach its own NotLinearized arm from here.
                Err(LinearizationCheckError::NotLinearized) => {
                    return Err(malformed!(
                        "not a linearized PDF: the first object in the file has no /Linearized key"
                    )
                    .into());
                } // cov:ignore-end
            }

            Ok((params, page_offset, shared_object, outline))
        })();

    match tables {
        Ok((params, page_offset, shared_object, outline)) => Ok(ShowLinearizationOutput {
            dump: format_dump(
                display_name,
                &params,
                &page_offset,
                &shared_object,
                outline.as_ref(),
            ),
            warnings,
        }),
        Err(ShowTablesError::QpdfDamage {
            object,
            offset,
            detail,
        }) => Ok(ShowLinearizationOutput {
            dump: String::new(),
            warnings: vec![format!(
                "{display_name} ({object}, offset {offset}): {detail}"
            )],
        }),
        Err(ShowTablesError::Other(ShowLinearizationError::Malformed { message })) => {
            Ok(ShowLinearizationOutput {
                dump: String::new(),
                // cov:ignore-start: linearization_parameter_warning's only Err
                // arm is a source I/O failure from linearization_candidate_ref,
                // and a Cursor<Vec<u8>> cannot produce one (see the identical
                // reasoning on the Io arm just below).
                warnings: vec![linearization_parameter_warning(
                    pdf,
                    display_name,
                    &message,
                )?],
                // cov:ignore-end
            })
        }
        Err(ShowTablesError::Other(err @ ShowLinearizationError::Io(_))) => Err(err), // cov:ignore: Cursor<Vec<u8>> show inputs cannot produce this generic source-I/O arm
    }
}

/// Assemble the complete dump string (qpdf's `dumpLinearizationDataInternal`).
fn format_dump(
    display_name: &str,
    p: &LinParameters,
    page_offset: &HPageOffset,
    shared_object: &HSharedObject,
    outline: Option<&HGeneric>,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = write!(out, "{display_name}: linearization data:\n\n");
    let _ = write!(
        out,
        "file_size: {}\n\
         first_page_object: {}\n\
         first_page_end: {}\n\
         npages: {}\n\
         xref_zero_offset: {}\n\
         first_page: {}\n\
         H_offset: {}\n\
         H_length: {}\n\n",
        p.file_size,
        p.first_page_object,
        p.first_page_end,
        p.npages,
        p.xref_zero_offset,
        p.first_page,
        p.h_offset,
        p.h_length,
    );

    out.push_str("Page Offsets Hint Table\n\n");
    dump_page_offset(&mut out, p, page_offset);
    out.push_str("\nShared Objects Hint Table\n\n");
    dump_shared_object(&mut out, p, shared_object);

    if let Some(g) = outline {
        if g.nobjects > 0 {
            out.push_str("\nOutlines Hint Table\n\n");
            dump_generic(&mut out, p, g);
        }
    }

    out
}

/// Parse an `N G obj` header at the start of `window` and return its
/// [`ObjectRef`].  Mirrors the strict header parser used by the checker.
#[cfg(test)]
fn parse_obj_header(window: &[u8]) -> Option<ObjectRef> {
    fn is_ws(b: u8) -> bool {
        matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
    }
    let mut i = 0;
    while i < window.len() && is_ws(window[i]) {
        i += 1;
    }
    let num_start = i;
    while i < window.len() && window[i].is_ascii_digit() {
        i += 1;
    }
    if i == num_start {
        return None;
    }
    let num: u32 = std::str::from_utf8(&window[num_start..i])
        .ok()?
        .parse()
        .ok()?;
    if i >= window.len() || !is_ws(window[i]) {
        return None;
    }
    while i < window.len() && is_ws(window[i]) {
        i += 1;
    }
    let gen_start = i;
    while i < window.len() && window[i].is_ascii_digit() {
        i += 1;
    }
    if i == gen_start {
        return None;
    }
    let gen: u16 = std::str::from_utf8(&window[gen_start..i])
        .ok()?
        .parse()
        .ok()?;
    if i >= window.len() || !is_ws(window[i]) {
        return None;
    }
    while i < window.len() && is_ws(window[i]) {
        i += 1;
    }
    if window.get(i..i + 3) != Some(b"obj") {
        return None;
    }
    Some(ObjectRef::new(num, gen))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Decode the linearization data of a PDF given its raw bytes and format it like
/// qpdf `--show-linearization`.
///
/// `display_name` is printed verbatim on the leading `"<name>: linearization
/// data:"` line, matching qpdf's echo of the path passed on the command line.
///
/// When the input is not linearized (the first object is missing, is not a
/// dictionary, has no numeric `/Linearized` whose floor is 1, or has an integer
/// `/L` that does not equal the file length), the returned string is
/// `"<display_name> is not linearized\n"` — this is not an error, mirroring
/// qpdf which prints that line to stdout and exits 0.
///
/// A linearized file whose parameter dictionary or hint stream is malformed
/// (a required value is missing or has the wrong type, the hint stream
/// cannot be located or decoded, a hint-stream `/S` or `/O` offset is out of
/// bounds, or the bit stream is truncated) is not an error either: mirroring
/// qpdf's `showLinearizationData`, which catches every such failure and
/// reports it as a single warning instead of aborting, the returned string
/// is empty in that case. Use
/// [`show_linearization_bytes_with_warnings`] to see the warning message.
///
/// # Errors
///
/// Returns [`ShowLinearizationError::Io`] when opening the [`Pdf`] from the
/// in-memory bytes or resolving an object fails.
pub fn show_linearization_bytes(
    file_bytes: &[u8],
    display_name: &str,
) -> std::result::Result<String, ShowLinearizationError> {
    show_linearization_bytes_with_warnings(file_bytes, display_name).map(|result| result.dump)
}

/// Decode linearization data and retain qpdf's ordered soft-check warnings.
pub fn show_linearization_bytes_with_warnings(
    file_bytes: &[u8],
    display_name: &str,
) -> std::result::Result<ShowLinearizationOutput, ShowLinearizationError> {
    let mut pdf = Pdf::open(Cursor::new(file_bytes.to_vec()))
        .map_err(|e| ShowLinearizationError::Io(Box::new(e)))?;
    show_with_pdf(&mut pdf, file_bytes, display_name)
}

/// Decode the linearization data of the PDF at `path` and format it like qpdf
/// `--show-linearization`.
///
/// The leading filename line echoes `path` exactly as given (matching qpdf,
/// which prints the path passed on its command line).
///
/// # Errors
///
/// Returns [`ShowLinearizationError::Io`] when reading the file at `path` or
/// opening the [`Pdf`] fails. Otherwise propagates any error from
/// [`show_linearization_bytes`] (see its documentation; a non-linearized file
/// is reported as an `Ok` "is not linearized" line, not an error).
pub fn show_linearization_path(
    path: &std::path::Path,
) -> std::result::Result<String, ShowLinearizationError> {
    show_linearization_path_with_warnings(path).map(|result| result.dump)
}

/// Decode linearization data at `path` and retain qpdf's ordered soft-check
/// warnings alongside the dump.
pub fn show_linearization_path_with_warnings(
    path: &std::path::Path,
) -> std::result::Result<ShowLinearizationOutput, ShowLinearizationError> {
    let file_bytes = std::fs::read(path)?;
    let mut pdf = Pdf::open(Cursor::new(file_bytes.clone()))
        .map_err(|e| ShowLinearizationError::Io(Box::new(e)))?;
    let display_name = path.to_string_lossy();
    show_with_pdf(&mut pdf, &file_bytes, &display_name)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bit_writer::BitWriter;
    use crate::linearization::hint_stream::encode_hint_stream;
    use crate::pipeline::buffer::Buffer;
    use crate::pipeline::{Pipeline, PipelineResult};

    /// Build a show-specific synthetic hint fixture through the production
    /// bit-writer pipeline.
    fn bit_writer_bytes(write: impl FnOnce(&mut BitWriter<'_>) -> PipelineResult<()>) -> Vec<u8> {
        let mut sink = Buffer::new("show test bit writer", None);
        {
            let mut writer = BitWriter::new(&mut sink);
            write(&mut writer).expect("write test bitstream");
            writer.flush().expect("flush test bitstream");
        }
        sink.finish().expect("finish test bitstream");
        sink.take_buffer().expect("take test bitstream")
    }

    // -----------------------------------------------------------------------
    // Round-trip: build qpdf-shaped tables with NON-ZERO bit widths and varied
    // per-page / per-shared values, encode via the production encoder, decode,
    // and assert the decoded fields equal the originals (decode∘encode == id).
    // -----------------------------------------------------------------------

    use crate::linearization::hint_page::{
        bits_needed, PageOffsetEntry, PageOffsetHeader, PageOffsetHintTable,
    };
    use crate::linearization::hint_shared::{
        SharedGroupEntry, SharedObjectEntry, SharedObjectHeader, SharedObjectHintTable,
    };

    /// A page-offset table with varied, non-zero values that force cross-byte
    /// bit reads in every column.
    fn rich_page_offset_table() -> PageOffsetHintTable {
        // Per-page object counts: 2, 5, 3 → least 2, delta max 3 → 2 bits.
        // Page lengths (minus least): 0, 130, 7 → 8 bits.
        // Content offsets (minus least): 0, 9, 3 → 4 bits.
        // Content lengths (minus least): 0, 130, 7 → 8 bits.
        // nshared per page: 0, 2, 1 → 2 bits.
        // shared identifiers: up to 5 → 3 bits. numerators: up to 3 → 2 bits.
        let header = PageOffsetHeader {
            least_object_count: 2,
            location_of_first_page: 1000,
            bits_object_count_delta: 2,
            least_page_length: 50,
            bits_page_length_delta: 8,
            least_content_offset: 4,
            bits_content_offset_delta: 4,
            least_content_length: 50,
            bits_content_length_delta: 8,
            bits_shared_object_count: 2,
            bits_shared_object_id: 3,
            bits_numerator: 2,
            denominator: 4,
        };
        let entries = vec![
            PageOffsetEntry {
                object_count_minus_least: 0,
                page_length_minus_least: 0,
                shared_object_count: 0,
                shared_object_ids: vec![],
                shared_object_numerators: vec![],
                content_stream_offset: 0,
                content_stream_length: 0,
            },
            PageOffsetEntry {
                object_count_minus_least: 3,
                page_length_minus_least: 130,
                shared_object_count: 2,
                shared_object_ids: vec![5, 1],
                shared_object_numerators: vec![3, 0],
                content_stream_offset: 9,
                content_stream_length: 130,
            },
            PageOffsetEntry {
                object_count_minus_least: 1,
                page_length_minus_least: 7,
                shared_object_count: 1,
                shared_object_ids: vec![4],
                shared_object_numerators: vec![2],
                content_stream_offset: 3,
                content_stream_length: 7,
            },
        ];
        PageOffsetHintTable { header, entries }
    }

    /// A shared-object table with varied group lengths (8-bit delta) and a
    /// non-empty Part-8 region (first_shared_obj / nshared_first_page set).
    fn rich_shared_object_table() -> SharedObjectHintTable {
        let header = SharedObjectHeader {
            first_object_number: 17,
            location: 2000,
            first_page_entries: 2,
            section_entries: 3,
            bits_group_object_count: 0,
            least_length: 33,
            bits_length_delta: 8,
        };
        let groups = vec![SharedGroupEntry { object_count: 1 }; 3];
        let objects = vec![
            SharedObjectEntry {
                length_minus_least: 0,
                signature_present: false,
                signature: None,
                nobjects_minus_one: 0,
            },
            SharedObjectEntry {
                length_minus_least: 157,
                signature_present: false,
                signature: None,
                nobjects_minus_one: 0,
            },
            SharedObjectEntry {
                length_minus_least: 75,
                signature_present: false,
                signature: None,
                nobjects_minus_one: 0,
            },
        ];
        SharedObjectHintTable {
            header,
            groups,
            objects,
        }
    }

    #[test]
    fn round_trip_page_offset_table() {
        let po = rich_page_offset_table();
        let so = rich_shared_object_table();
        let encoded = encode_hint_stream(&po, &so, None).expect("encode");

        let decoded = read_h_page_offset(&encoded.uncompressed, po.entries.len() as u32)
            .expect("decode page offset");

        assert_eq!(decoded.min_nobjects, po.header.least_object_count);
        assert_eq!(decoded.first_page_offset, po.header.location_of_first_page);
        assert_eq!(
            decoded.nbits_delta_nobjects,
            po.header.bits_object_count_delta
        );
        assert_eq!(decoded.min_page_length, po.header.least_page_length);
        assert_eq!(
            decoded.nbits_delta_page_length,
            po.header.bits_page_length_delta
        );
        assert_eq!(decoded.min_content_offset, po.header.least_content_offset);
        assert_eq!(
            decoded.nbits_delta_content_offset,
            po.header.bits_content_offset_delta
        );
        assert_eq!(decoded.min_content_length, po.header.least_content_length);
        assert_eq!(
            decoded.nbits_delta_content_length,
            po.header.bits_content_length_delta
        );
        assert_eq!(
            decoded.nbits_nshared_objects,
            po.header.bits_shared_object_count
        );
        assert_eq!(
            decoded.nbits_shared_identifier,
            po.header.bits_shared_object_id
        );
        assert_eq!(decoded.nbits_shared_numerator, po.header.bits_numerator);
        assert_eq!(decoded.shared_denominator, po.header.denominator);

        assert_eq!(decoded.entries.len(), po.entries.len());
        for (d, o) in decoded.entries.iter().zip(po.entries.iter()) {
            assert_eq!(d.delta_nobjects, o.object_count_minus_least as u64);
            assert_eq!(d.delta_page_length, o.page_length_minus_least);
            assert_eq!(d.nshared_objects, o.shared_object_count);
            assert_eq!(
                d.shared_identifiers,
                o.shared_object_ids
                    .iter()
                    .map(|&x| x as u64)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                d.shared_numerators,
                o.shared_object_numerators
                    .iter()
                    .map(|&x| x as u64)
                    .collect::<Vec<_>>()
            );
            assert_eq!(d.delta_content_offset, o.content_stream_offset);
            assert_eq!(d.delta_content_length, o.content_stream_length);
        }
    }

    #[test]
    fn round_trip_shared_object_table() {
        let po = rich_page_offset_table();
        let so = rich_shared_object_table();
        let encoded = encode_hint_stream(&po, &so, None).expect("encode");

        let decoded = read_h_shared_object(
            &encoded.uncompressed[encoded.shared_section_offset_in_uncompressed..],
        )
        .expect("decode shared object");

        assert_eq!(
            decoded.first_shared_obj,
            so.header.first_object_number as u64
        );
        assert_eq!(decoded.first_shared_offset, so.header.location);
        assert_eq!(decoded.nshared_first_page, so.header.first_page_entries);
        assert_eq!(decoded.nshared_total, so.header.section_entries);
        assert_eq!(decoded.nbits_nobjects, so.header.bits_group_object_count);
        assert_eq!(decoded.min_group_length, so.header.least_length);
        assert_eq!(
            decoded.nbits_delta_group_length,
            so.header.bits_length_delta
        );

        assert_eq!(decoded.entries.len(), so.objects.len());
        for (d, o) in decoded.entries.iter().zip(so.objects.iter()) {
            assert_eq!(d.delta_group_length, o.length_minus_least as u64);
            assert_eq!(d.signature_present, o.signature_present);
            assert_eq!(d.nobjects_minus_one, o.nobjects_minus_one as u64);
        }
    }

    #[test]
    fn round_trip_uses_nonzero_bit_widths() {
        // Guard: ensure the fixture actually forces multi-bit reads (so the
        // round-trip is a real cross-byte test, not all-zero placeholders).
        let po = rich_page_offset_table();
        assert!(po.header.bits_page_length_delta >= 8);
        assert_eq!(bits_needed(130), 8);
    }

    // -----------------------------------------------------------------------
    // adjusted_offset: offsets >= H_offset gain H_length; others unchanged.
    // -----------------------------------------------------------------------

    #[test]
    fn adjusted_offset_adds_h_length_at_or_above_h_offset() {
        let p = LinParameters {
            file_size: 0,
            first_page_object: 0,
            first_page_end: 0,
            npages: 0,
            xref_zero_offset: 0,
            first_page: 0,
            h_offset: 100,
            h_length: 50,
            h_overflow_offset: 0,
            h_overflow_length: 0,
        };
        assert_eq!(p.adjusted_offset(99), 99); // below H_offset: unchanged
        assert_eq!(p.adjusted_offset(100), 150); // at H_offset: + H_length
        assert_eq!(p.adjusted_offset(200), 250); // above: + H_length
    }

    // -----------------------------------------------------------------------
    // Outlines dump: only emitted when nobjects > 0; offset is adjusted.
    // -----------------------------------------------------------------------

    #[test]
    fn outlines_section_emitted_only_when_nobjects_positive() {
        let p = LinParameters {
            file_size: 10,
            first_page_object: 1,
            first_page_end: 5,
            npages: 1,
            xref_zero_offset: 8,
            first_page: 0,
            h_offset: 100,
            h_length: 50,
            h_overflow_offset: 0,
            h_overflow_length: 0,
        };
        let po = HPageOffset {
            min_nobjects: 1,
            first_page_offset: 0,
            nbits_delta_nobjects: 0,
            min_page_length: 0,
            nbits_delta_page_length: 0,
            min_content_offset: 0,
            nbits_delta_content_offset: 0,
            min_content_length: 0,
            nbits_delta_content_length: 0,
            nbits_nshared_objects: 0,
            nbits_shared_identifier: 0,
            nbits_shared_numerator: 0,
            shared_denominator: 4,
            entries: vec![HPageOffsetEntry {
                delta_nobjects: 0,
                delta_page_length: 0,
                nshared_objects: 0,
                shared_identifiers: vec![],
                shared_numerators: vec![],
                delta_content_offset: 0,
                delta_content_length: 0,
            }],
        };
        let so = HSharedObject {
            first_shared_obj: 0,
            first_shared_offset: 0,
            nshared_first_page: 0,
            nshared_total: 0,
            nbits_nobjects: 0,
            min_group_length: 0,
            nbits_delta_group_length: 0,
            entries: vec![],
        };

        // nobjects == 0 → no Outlines section.
        let empty_outline = HGeneric {
            first_object: 0,
            first_object_offset: 0,
            nobjects: 0,
            group_length: 0,
        };
        let dump_no_outline = format_dump("f.pdf", &p, &po, &so, Some(&empty_outline));
        assert!(!dump_no_outline.contains("Outlines Hint Table"));

        // nobjects > 0 → Outlines section present with adjusted offset.
        let outline = HGeneric {
            first_object: 30,
            first_object_offset: 120, // >= H_offset(100) → +H_length(50) = 170
            nobjects: 2,
            group_length: 99,
        };
        let dump_outline = format_dump("f.pdf", &p, &po, &so, Some(&outline));
        assert!(dump_outline.contains("\nOutlines Hint Table\n\n"));
        assert!(dump_outline.contains("first_object: 30\n"));
        assert!(dump_outline.contains("first_object_offset: 170\n"));
        assert!(dump_outline.contains("nobjects: 2\n"));
        assert!(dump_outline.contains("group_length: 99\n"));
    }

    // -----------------------------------------------------------------------
    // Shared-object dump: "signature present" / "nobjects:" conditionals.
    // -----------------------------------------------------------------------

    #[test]
    fn shared_object_conditional_lines() {
        let p = LinParameters {
            file_size: 0,
            first_page_object: 0,
            first_page_end: 0,
            npages: 0,
            xref_zero_offset: 0,
            first_page: 0,
            h_offset: 0,
            h_length: 0,
            h_overflow_offset: 0,
            h_overflow_length: 0,
        };
        let t = HSharedObject {
            first_shared_obj: 0,
            first_shared_offset: 0,
            nshared_first_page: 2,
            nshared_total: 2,
            nbits_nobjects: 1,
            min_group_length: 10,
            nbits_delta_group_length: 0,
            entries: vec![
                // signature present + nobjects > 1
                HSharedObjectEntry {
                    delta_group_length: 0,
                    signature_present: true,
                    nobjects_minus_one: 2,
                },
                // neither
                HSharedObjectEntry {
                    delta_group_length: 5,
                    signature_present: false,
                    nobjects_minus_one: 0,
                },
            ],
        };
        let mut out = String::new();
        dump_shared_object(&mut out, &p, &t);
        assert!(out.contains(
            "Shared Object 0:\n  group length: 10\n  signature present\n  nobjects: 3\n"
        ));
        // Entry 1: no signature, no nobjects line.
        assert!(out.contains("Shared Object 1:\n  group length: 15\n"));
        assert!(!out.contains("Shared Object 1:\n  group length: 15\n  signature present"));
    }

    // -----------------------------------------------------------------------
    // signature_present + 128-bit skip (qpdf readHSharedObject column c).
    //
    // qpdf's reader expects four SEPARATE columns: delta_group_length, then a
    // 1-bit signature_present column (byte-aligned), then a run of 128-bit
    // signatures (one per set flag, no inner alignment), then nobjects. We
    // build the bitstream in that exact reader order via BitWriter and
    // assert the decoder skips the signature correctly so the nobjects column
    // stays aligned.  (flpdf never emits signatures, so this path is exercised
    // only here, but it must mirror qpdf for arbitrary linearized input.)
    // -----------------------------------------------------------------------

    #[test]
    fn shared_object_signature_present_skips_128_bits() {
        let nbits_delta_group_length = 4u32;
        let nbits_nobjects = 4u32;
        let nshared_total = 2u32;

        let buf = bit_writer_bytes(|writer| {
            // 7-field header (32-bit ×5, 16-bit ×2 = 24 bytes, byte-aligned).
            writer.write_bits(1, 32)?; // first_shared_obj
            writer.write_bits(0, 32)?; // first_shared_offset
            writer.write_bits(2, 32)?; // nshared_first_page
            writer.write_bits(nshared_total as u64, 32)?; // nshared_total
            writer.write_bits(nbits_nobjects as u64, 16)?; // nbits_nobjects
            writer.write_bits(0, 32)?; // min_group_length
            writer.write_bits(nbits_delta_group_length as u64, 16)?; // nbits_delta_group_length
                                                                     // col a: delta_group_length × N
            writer.write_bits(3, nbits_delta_group_length as usize)?;
            writer.write_bits(9, nbits_delta_group_length as usize)?;
            writer.flush()?;
            // col b: signature_present × N (entry 0 set, entry 1 clear)
            writer.write_bits(1, 1)?;
            writer.write_bits(0, 1)?;
            writer.flush()?;
            // col c: 128 bits (4×32) for the one set flag — no inner alignment
            for _ in 0..4 {
                writer.write_bits(0xDEAD_BEEF, 32)?;
            }
            // col d: nobjects_minus_one × N
            writer.write_bits(5, nbits_nobjects as usize)?;
            writer.write_bits(7, nbits_nobjects as usize)?;
            writer.flush()
        });

        let decoded = read_h_shared_object(&buf).expect("decode");
        assert!(decoded.entries[0].signature_present);
        assert_eq!(decoded.entries[0].delta_group_length, 3);
        assert_eq!(decoded.entries[0].nobjects_minus_one, 5);
        assert!(!decoded.entries[1].signature_present);
        assert_eq!(decoded.entries[1].delta_group_length, 9);
        // The decoder must have skipped exactly 128 bits, leaving the nobjects
        // column aligned — proven by entry 1 decoding to 7, not garbage.
        assert_eq!(decoded.entries[1].nobjects_minus_one, 7);
    }

    #[test]
    fn shared_object_oversized_nshared_total_is_malformed() {
        // A 24-byte header (byte-aligned) claiming a huge nshared_total but with
        // no column bytes following. Each entry needs at least one bit (the
        // signature_present column), so the decoder must reject it as malformed
        // rather than pre-allocating ~u32::MAX entries (OOM DoS guard).
        let buf = bit_writer_bytes(|writer| {
            writer.write_bits(0, 32)?; // first_shared_obj
            writer.write_bits(0, 32)?; // first_shared_offset
            writer.write_bits(0, 32)?; // nshared_first_page
            writer.write_bits(1_000_000, 32)?; // nshared_total
            writer.write_bits(0, 16)?; // nbits_nobjects
            writer.write_bits(0, 32)?; // min_group_length
            writer.write_bits(0, 16) // nbits_delta_group_length
        });
        assert_eq!(buf.len(), 24, "header is exactly 24 bytes, no column data");

        let is_malformed = matches!(
            read_h_shared_object(&buf),
            Err(ShowLinearizationError::Malformed { .. })
        );
        assert!(
            is_malformed,
            "expected Malformed for oversized nshared_total"
        );
    }

    #[test]
    fn page_offset_oversized_nshared_objects_is_malformed() {
        // A single-page table whose `nshared_objects` column claims ~1e6 shared
        // refs, but the stream ends right after that column. With zero-width
        // identifier/numerator fields the nested loops would push ~1e6 entries
        // from a tiny buffer; the per-page bound must reject it instead.
        let buf = bit_writer_bytes(|writer| {
            // 13-field header (5×32 + 8×16 = 36 bytes, byte-aligned).
            writer.write_bits(0, 32)?; // min_nobjects
            writer.write_bits(0, 32)?; // first_page_offset
            writer.write_bits(0, 16)?; // nbits_delta_nobjects = 0
            writer.write_bits(0, 32)?; // min_page_length
            writer.write_bits(0, 16)?; // nbits_delta_page_length = 0
            writer.write_bits(0, 32)?; // min_content_offset
            writer.write_bits(0, 16)?; // nbits_delta_content_offset = 0
            writer.write_bits(0, 32)?; // min_content_length
            writer.write_bits(0, 16)?; // nbits_delta_content_length = 0
            writer.write_bits(32, 16)?; // nbits_nshared_objects = 32
            writer.write_bits(0, 16)?; // nbits_shared_identifier = 0
            writer.write_bits(0, 16)?; // nbits_shared_numerator = 0
            writer.write_bits(1, 16)?; // shared_denominator
                                       // cols (a)/(b): 0-bit, nothing written. col (c):
                                       // nshared_objects for the single page.
            writer.write_bits(1_000_000, 32)?;
            writer.flush()
        });
        assert_eq!(
            buf.len(),
            40,
            "36-byte header + 4-byte nshared_objects column"
        );

        let is_malformed = matches!(
            read_h_page_offset(&buf, 1),
            Err(ShowLinearizationError::Malformed { .. })
        );
        assert!(
            is_malformed,
            "expected Malformed for oversized per-page nshared_objects"
        );
    }

    // -----------------------------------------------------------------------
    // Error arms on the byte-level API.
    // -----------------------------------------------------------------------

    #[test]
    fn truncated_hint_uses_bit_stream_error_text() {
        let err = read_h_page_offset(&[0xff], 1)
            .err()
            .expect("truncated hint must fail");
        assert!(err.to_string().contains("overflow reading bit stream"));
    }

    /// A minimal, valid but non-linearized PDF.
    fn non_linearized_pdf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn show_bytes_reports_non_linearized_as_ok_line() {
        // qpdf prints "<name> is not linearized" to stdout (exit 0), so this is
        // an Ok value, not an error.
        let pdf = non_linearized_pdf();
        let out = show_linearization_bytes(&pdf, "x.pdf").expect("not-linearized is Ok");
        assert_eq!(out, "x.pdf is not linearized\n");
    }

    #[test]
    fn show_bytes_first_object_not_a_dict_is_not_linearized() {
        // First physical object is an integer, not a dictionary → not
        // linearized (hits the `as_dict()` None branch).
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n42\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
        let off4 = pdf.len();
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len();
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n\
             {off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 2 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        let out = show_linearization_bytes(&pdf, "intfirst.pdf").expect("not-linearized is Ok");
        assert_eq!(out, "intfirst.pdf is not linearized\n");
    }

    #[test]
    fn param_u64_rejects_fractional() {
        let d = ObjectHandle::dictionary(vec![(b"/N".to_vec(), ObjectHandle::real(1.5))]);
        assert!(matches!(
            param_u64(&d, "N"),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    /// qpdf rejects a whole-number real literal for a deeper linearization
    /// parameter because `readLinearizationData` requires `isInteger()`.
    #[test]
    fn param_u64_rejects_real_literal_whole_number() {
        let d = ObjectHandle::dictionary(vec![(
            b"/N".to_vec(),
            ObjectHandle::real_literal(5.0, b"5.0".to_vec()),
        )]);
        assert!(matches!(
            param_u64(&d, "N"),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    /// A fractional `RealLiteral` (e.g. `1.5`) is rejected as a non-integer
    /// qpdf parameter.
    #[test]
    fn param_u64_rejects_fractional_real_literal() {
        let d = ObjectHandle::dictionary(vec![(
            b"/N".to_vec(),
            ObjectHandle::real_literal(1.5, b"1.5".to_vec()),
        )]);
        assert!(matches!(
            param_u64(&d, "N"),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn parse_obj_header_basic() {
        assert_eq!(
            parse_obj_header(b"7 0 obj\n<<>>"),
            Some(ObjectRef::new(7, 0))
        );
        assert_eq!(parse_obj_header(b"not an obj"), None);
    }

    // -----------------------------------------------------------------------
    // read_h_generic (Outlines table): no golden exercises it, so decode a
    // hand-built bitstream and confirm the four fields.
    // -----------------------------------------------------------------------

    #[test]
    fn read_h_generic_decodes_four_fields() {
        let buf = bit_writer_bytes(|writer| {
            writer.write_bits(11, 32)?; // first_object
            writer.write_bits(222, 32)?; // first_object_offset
            writer.write_bits(3, 32)?; // nobjects
            writer.write_bits(4444, 32) // group_length
        });
        let g = read_h_generic(&buf).expect("decode generic");
        assert_eq!(g.first_object, 11);
        assert_eq!(g.first_object_offset, 222);
        assert_eq!(g.nobjects, 3);
        assert_eq!(g.group_length, 4444);
    }

    #[test]
    fn read_h_generic_truncated_errors() {
        assert!(read_h_generic(&[0u8; 4]).is_err());
    }

    // -----------------------------------------------------------------------
    // Error type: Display and From conversions.
    // -----------------------------------------------------------------------

    #[test]
    fn error_display_and_from_conversions() {
        let m = ShowLinearizationError::Malformed {
            message: "boom".into(),
        };
        assert_eq!(format!("{m}"), "malformed linearization data: boom");

        let io: ShowLinearizationError = std::io::Error::other("disk gone").into();
        assert!(matches!(io, ShowLinearizationError::Io(_)));
        assert!(format!("{io}").starts_with("I/O error: "));

        let core: ShowLinearizationError = crate::Error::Missing("X").into();
        assert!(matches!(core, ShowLinearizationError::Io(_)));
    }

    // -----------------------------------------------------------------------
    // Whole-pipeline happy path + error arms, driven through real linearized
    // bytes built by the writer (same approach as check.rs's tests).
    // -----------------------------------------------------------------------

    use crate::linearization::plan::LinearizationPlan;
    use crate::linearization::renumber::RenumberMap;
    use crate::linearization::writer::write_linearized;
    use crate::writer::WriterOptions;
    use crate::Pdf;
    use std::io::Cursor;

    /// Minimal single-page non-linearized source PDF.
    fn tiny_source() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n\
             {off3:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// Build a real, back-patched linearized PDF from `tiny_source`.
    fn linearized_bytes() -> Vec<u8> {
        let raw = tiny_source();
        let mut pdf = Pdf::open(Cursor::new(raw.clone())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(raw)).unwrap();
        let mut doc =
            write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default()).unwrap();
        doc.back_patch().unwrap();
        doc.bytes
    }

    fn compat_linearized_bytes() -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/compat/linearized-one-page.pdf"),
        )
        .expect("linearized fixture")
    }

    fn replace_numeric_value(bytes: &mut [u8], key: &[u8], replacement: &[u8]) {
        let key_pos = bytes
            .windows(key.len())
            .position(|window| window == key)
            .expect("linearization numeric key");
        let mut start = key_pos + key.len();
        while bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        let end = start
            + bytes[start..]
                .iter()
                .position(|&byte| byte.is_ascii_whitespace())
                .expect("/H[0] value");
        assert!(replacement.len() <= end - start);
        let mut padded = vec![b' '; end - start];
        padded[..replacement.len()].copy_from_slice(replacement);
        bytes[start..end].copy_from_slice(&padded);
    }

    fn replace_hint_offset(bytes: &mut [u8], replacement: &[u8]) {
        replace_numeric_value(bytes, b"/H [", replacement);
    }

    #[test]
    fn show_linearized_happy_path_produces_dump() {
        let bytes = linearized_bytes();
        let out = show_linearization_bytes(&bytes, "lin.pdf").expect("dump");
        assert!(out.starts_with("lin.pdf: linearization data:\n\n"));
        assert!(out.contains("\nPage Offsets Hint Table\n\n"));
        assert!(out.contains("\nShared Objects Hint Table\n\n"));
        // npages == 1 → exactly one "Page 0:" header.
        assert!(out.contains("Page 0:\n"));
        assert!(!out.contains("Page 1:\n"));
    }

    /// Regression test for the P2 finding on the `check_linearization` call
    /// in this function: qpdf's `showLinearizationData` never branches on
    /// `checkLinearizationInternal`'s boolean result
    /// (`QPDF_linearization.cc:838-843`) — a soft mismatch only accumulates a
    /// `linearizationWarning`, it never stops `dumpLinearizationDataInternal`
    /// from running. Tamper `/O` to point at object 0 (always free/reserved,
    /// never a real indirect object) so `check_linearization` still reports
    /// a mismatch on its own, but `show_linearization_bytes` on the exact
    /// same bytes must still produce the complete table dump.
    #[test]
    fn show_continues_past_a_qpdf_soft_check_mismatch() {
        let mut bytes = linearized_bytes();
        let key_pos = bytes
            .windows(3)
            .position(|window| window == b"/O ")
            .expect("/O key");
        let start = key_pos + 3;
        let end = start
            + bytes[start..]
                .iter()
                .position(|&byte| !byte.is_ascii_digit())
                .expect("/O numeric value");
        let width = end - start;
        let mut padded = vec![b' '; width];
        padded[0] = b'0';
        bytes[start..end].copy_from_slice(&padded);

        let check_result = super::super::check::check_linearization_bytes(&bytes);
        assert!(
            check_result.is_err(),
            "tampered /O must still fail the standalone check"
        );

        let out = show_linearization_bytes(&bytes, "tampered.pdf")
            .expect("show must still produce a dump despite the /O mismatch");
        assert!(out.starts_with("tampered.pdf: linearization data:\n\n"));
        assert!(out.contains("\nPage Offsets Hint Table\n\n"));
        assert!(out.contains("\nShared Objects Hint Table\n\n"));
    }

    #[test]
    fn show_loads_the_four_item_overflow_hint_stream() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/compat/linearized-one-page.pdf"),
        )
        .expect("linearized fixture");
        let bytes = crate::linearization::check::add_overflow_hint_items_for_test(bytes);
        let out = show_linearization_bytes(&bytes, "overflow.pdf").expect("overflow dump");
        assert!(out.starts_with("overflow.pdf: linearization data:\n\n"));
    }

    /// Compress `data` with the same production Flate pipeline `encode_hint_stream`
    /// uses, for building a hand-rolled hint stream object's payload.
    fn deflate_compress(data: &[u8]) -> Vec<u8> {
        let mut sink = Buffer::new("test overflow hint compress", None);
        {
            let mut flate = crate::pipeline::flate::Flate::new(
                "test overflow hint compress",
                &mut sink,
                crate::pipeline::flate::FlateAction::Deflate,
                crate::pipeline::flate::DEFAULT_OUT_BUFFER_SIZE,
            )
            .expect("flate init");
            flate.write(data).expect("flate write");
            flate.finish().expect("flate finish");
        }
        sink.take_buffer().expect("compressed buffer")
    }

    /// Build a hand-rolled linearized PDF whose `/H` genuinely splits real
    /// hint-table data across two physical stream objects: object 5 (the
    /// primary) carries only the Page Offset Hint Table section, object 6
    /// (the overflow) carries only the Shared Object Hint Table section, and
    /// `/S` on the primary's dict is set to the split point in the
    /// concatenated buffer. Unlike `show_loads_the_four_item_overflow_hint_stream`
    /// (whose /H[2..3] point at the SAME complete primary stream — a
    /// degenerate case that never exercises real overflow), decoding the
    /// Shared Objects Hint Table here is only possible if the overflow
    /// stream's bytes are actually appended after the primary's.
    fn split_overflow_pdf_bytes() -> Vec<u8> {
        let po = PageOffsetHintTable {
            header: PageOffsetHeader {
                least_object_count: 1,
                location_of_first_page: 100,
                bits_object_count_delta: 8,
                least_page_length: 10,
                bits_page_length_delta: 8,
                least_content_offset: 0,
                bits_content_offset_delta: 8,
                least_content_length: 10,
                bits_content_length_delta: 8,
                bits_shared_object_count: 8,
                bits_shared_object_id: 8,
                bits_numerator: 8,
                denominator: 4,
            },
            entries: vec![PageOffsetEntry {
                object_count_minus_least: 0,
                page_length_minus_least: 0,
                shared_object_count: 0,
                shared_object_ids: vec![],
                shared_object_numerators: vec![],
                content_stream_offset: 0,
                content_stream_length: 0,
            }],
        };
        let so = SharedObjectHintTable {
            header: SharedObjectHeader {
                first_object_number: 5,
                location: 200,
                first_page_entries: 0,
                section_entries: 2,
                // 0: nobjects_minus_one is always 0 below, and (matching
                // `rich_shared_object_table`) a nonzero width here also
                // widens the writer's `groups` pre-pass
                // (`encode_shared_object_groups`, `hint_stream.rs:373-385`),
                // which qpdf's `readHSharedObject` has no matching read step
                // for — misaligning every subsequent bit read. Tracked
                // separately (flpdf-pars); production always passes 0 here
                // (`hint_shared.rs:242,347`) so this is currently dormant.
                bits_group_object_count: 0,
                least_length: 10,
                bits_length_delta: 8,
            },
            groups: vec![
                SharedGroupEntry { object_count: 1 },
                SharedGroupEntry { object_count: 1 },
            ],
            objects: vec![
                SharedObjectEntry {
                    length_minus_least: 5,
                    signature_present: false,
                    signature: None,
                    nobjects_minus_one: 0,
                },
                SharedObjectEntry {
                    length_minus_least: 20,
                    signature_present: false,
                    signature: None,
                    nobjects_minus_one: 0,
                },
            ],
        };
        let encoded = encode_hint_stream(&po, &so, None).expect("encode split hint tables");
        // The split point IS the /S value: everything before it is the Page
        // Offset section (goes in the primary stream), everything from it
        // onward is the Shared Object section (goes in the overflow stream).
        let split = encoded.shared_section_offset_in_uncompressed;
        assert!(
            split > 0 && split < encoded.uncompressed.len(),
            "fixture must have non-empty page and shared sections either side of /S"
        );
        let (page_section, shared_section) = encoded.uncompressed.split_at(split);
        let primary_compressed = deflate_compress(page_section);
        let overflow_compressed = deflate_compress(shared_section);

        // Fixed-width (10-digit, zero-padded) numeric placeholders in the
        // param dict so patching them after the rest of the file is built
        // cannot shift any later byte offset — the same technique check.rs's
        // own tests use for /L (`LINEARIZED_L_MARKER`).
        const WIDTH: usize = 10;
        fn push_placeholder(bytes: &mut Vec<u8>, slots: &mut Vec<(usize, usize)>) {
            let start = bytes.len();
            bytes.extend(std::iter::repeat_n(b'0', WIDTH));
            slots.push((start, bytes.len()));
        }
        fn patch(bytes: &mut [u8], slot: (usize, usize), value: u64) {
            let text = format!("{value:0width$}", width = WIDTH);
            bytes[slot.0..slot.1].copy_from_slice(text.as_bytes());
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");

        let mut slots = Vec::new(); // [L, H0off, H0len, H1off, H1len, E, T]
        bytes.extend_from_slice(b"1 0 obj\n<< /Linearized 1 /L ");
        push_placeholder(&mut bytes, &mut slots); // L
        bytes.extend_from_slice(b" /H [");
        push_placeholder(&mut bytes, &mut slots); // H0 offset
        bytes.push(b' ');
        push_placeholder(&mut bytes, &mut slots); // H0 length
        bytes.push(b' ');
        push_placeholder(&mut bytes, &mut slots); // H1 offset
        bytes.push(b' ');
        push_placeholder(&mut bytes, &mut slots); // H1 length
        bytes.extend_from_slice(b"] /O 4 /E ");
        push_placeholder(&mut bytes, &mut slots); // E
        bytes.extend_from_slice(b" /N 1 /T ");
        push_placeholder(&mut bytes, &mut slots); // T
        bytes.extend_from_slice(b" /P 0 >>\nendobj\n");

        let off2 = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
        let off3 = bytes.len();
        bytes.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
        let off4 = bytes.len();
        bytes.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let h0_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "5 0 obj\n<< /Length {} /Filter /FlateDecode /S {split} >>\nstream\n",
                primary_compressed.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&primary_compressed);
        bytes.extend_from_slice(b"\nendstream\nendobj");
        // end_before_space lands right after "endobj" (before its trailing
        // newline), matching `resolver.rs`'s `object_end_offsets`.
        let h0_length = (bytes.len() - h0_offset) as u64;
        bytes.push(b'\n');

        let off5 = bytes.len();
        let h1_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "6 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
                overflow_compressed.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&overflow_compressed);
        bytes.extend_from_slice(b"\nendstream\nendobj");
        let h1_length = (bytes.len() - h1_offset) as u64;
        bytes.push(b'\n');

        let xref_start = bytes.len();
        let xref = format!(
            "xref\n0 7\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n\
             {:010} 00000 n \n{h0_offset:010} 00000 n \n{off5:010} 00000 n \n",
            9, off2, off3, off4,
        );
        bytes.extend_from_slice(xref.as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 2 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let file_len = bytes.len() as u64;
        patch(&mut bytes, slots[0], file_len); // /L
        patch(&mut bytes, slots[1], h0_offset as u64);
        patch(&mut bytes, slots[2], h0_length);
        patch(&mut bytes, slots[3], h1_offset as u64);
        patch(&mut bytes, slots[4], h1_length);
        patch(&mut bytes, slots[5], 0); // /E — unused by this test
        patch(&mut bytes, slots[6], 0); // /T — unused by this test

        bytes
    }

    /// Regression test for the P2 finding that a genuine four-item `/H`
    /// discarded the overflow stream's decoded bytes: `decompressed` was
    /// always just the primary stream's buffer, so any table data that only
    /// exists in the overflow half was silently unavailable. qpdf's
    /// `readLinearizationData` pipes BOTH `readHintStream` calls into the
    /// SAME `Pl_Buffer` (`QPDF_linearization.cc:241-245`), so `/S` and `/O`
    /// index into the concatenation, not just the primary stream. Before the
    /// fix this failed with "hint stream /S offset ... is out of bounds",
    /// since `/S` (the split point) always pointed past the primary-only
    /// `decompressed.len()`.
    #[test]
    fn show_merges_a_genuine_two_stream_overflow_hint_table() {
        let bytes = split_overflow_pdf_bytes();

        let out = show_linearization_bytes(&bytes, "split.pdf")
            .expect("genuinely split /H overflow stream must merge into one dump");
        assert!(out.contains("\nShared Objects Hint Table\n\n"));
        // The Shared Objects Hint Table came from the overflow stream alone —
        // its two groups/lengths are only decodable if the merge actually
        // appended the overflow bytes. `least_length: 10` plus each object's
        // `length_minus_least` (5, 20) from `split_overflow_pdf_bytes`.
        assert!(out.contains("Shared Object 0:\n"));
        assert!(out.contains("Shared Object 1:\n"));
        assert!(out.contains("group length: 15\n"));
        assert!(out.contains("group length: 30\n"));
    }

    /// A corrupted overflow stream must still be reported, not silently
    /// ignored — the merge in `show_with_pdf` propagates a genuine decode
    /// failure from the overflow `load_hint_stream` call exactly like it
    /// already does for the primary stream (`decode_failure_is_malformed`).
    /// qpdf's `readHintStream` throw for this (`QPDF_linearization.cc:
    /// 284-322`) is caught by `showLinearizationData`'s single try/catch
    /// (`:837-846`) and becomes one bare warning with no dump, not a hard
    /// error — so this is `Ok`, not `Err`.
    #[test]
    fn show_reports_a_corrupted_overflow_hint_stream_as_malformed() {
        let mut bytes = split_overflow_pdf_bytes();
        let obj6 = bytes
            .windows(b"6 0 obj".len())
            .position(|window| window == b"6 0 obj")
            .expect("overflow hint stream object header");
        let stream_start = bytes[obj6..]
            .windows(b"stream\n".len())
            .position(|window| window == b"stream\n")
            .map(|relative| obj6 + relative + b"stream\n".len())
            .expect("overflow stream keyword");
        // Corrupt a byte a few bytes into the deflate payload (past the zlib
        // header), matching `decode_failure_is_malformed`'s technique.
        bytes[stream_start + 4] ^= 0xFF;

        let result = show_linearization_bytes_with_warnings(&bytes, "split-corrupt.pdf")
            .expect("a corrupted overflow hint stream is a qpdf warning, not a hard error");
        assert!(result.dump.is_empty());
        // The exact warning text depends on which DEFLATE backend rejects
        // the corrupted bytes first (miniz_oxide vs the qpdf-zlib-compat
        // feature's zlib): the pure-Rust decoder can produce a
        // still-truncated-but-parseable payload that fails at the
        // bit-stream layer instead, while zlib fails the inflate itself.
        // Both are genuine decode failures under the same qpdf throw site
        // (CLAUDE.md's sole byte-identical exception is DEFLATE output);
        // assert the shape (one warning, no dump), not the wording.
        assert_eq!(result.warnings.len(), 1, "warnings: {:?}", result.warnings);
    }

    #[test]
    fn show_linearization_path_round_trips_via_tempfile() {
        let bytes = linearized_bytes();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("flpdf-show-test-{}.pdf", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let out = show_linearization_path(&path).expect("path dump");
        let _ = std::fs::remove_file(&path);
        // The path form echoes the path on the first line.
        assert!(out.contains(": linearization data:\n\n"));
        assert!(out.contains(&*path.to_string_lossy()));
    }

    #[test]
    fn show_linearization_path_missing_file_is_io_error() {
        let path = std::path::Path::new("/nonexistent/flpdf/definitely/missing.pdf");
        assert!(matches!(
            show_linearization_path(path),
            Err(ShowLinearizationError::Io(_))
        ));
    }

    #[test]
    fn bad_l_value_is_not_linearized() {
        // Corrupt /L so it no longer equals the file length, keeping the same
        // byte width (bump the last digit with wrap) → qpdf treats this as not
        // linearized (isLinearized returns false on /L mismatch).
        let mut bytes = linearized_bytes();
        let pos = bytes.windows(3).position(|w| w == b"/L ").expect("/L");
        let dstart = pos + 3;
        let dend = dstart
            + bytes[dstart..]
                .iter()
                .position(|&b| !b.is_ascii_digit())
                .unwrap();
        // Flip the low bit of the last digit: 0↔1, 2↔3, …, 8↔9 — always a
        // different ASCII digit, so /L changes with no branch to leave
        // half-covered.
        let last = dend - 1;
        bytes[last] ^= 1;
        let out = show_linearization_bytes(&bytes, "badL.pdf").expect("not-linearized is Ok");
        assert_eq!(out, "badL.pdf is not linearized\n");
    }

    #[test]
    fn t_position_mismatch_is_a_warning_with_dump_intact() {
        // qpdf's own /T check (QPDF_linearization.cc:452-470) is a soft
        // warning inside checkLinearizationInternal, which never throws: it
        // only seeks to /T, skips whitespace, and compares the resulting
        // position against the xref parser's first_xref_item_offset
        // (QPDF.cc:845-869,1110-1120). A /T value that does not match that
        // position must therefore still produce a normal dump, with the
        // mismatch appended as a warning rather than replacing the dump with
        // nothing (the way a genuine readLinearizationData throw does).
        let mut bytes = linearized_bytes();
        let pos = bytes.windows(3).position(|w| w == b"/T ").expect("/T");
        let dstart = pos + 3;
        let dend = dstart
            + bytes[dstart..]
                .iter()
                .position(|&b| !b.is_ascii_digit())
                .expect("/T numeric value");
        let width = dend - dstart;
        bytes[dstart..dend].copy_from_slice(&vec![b'0'; width]); // /T = 0

        let result = show_linearization_bytes_with_warnings(&bytes, "badT.pdf")
            .expect("a /T position mismatch is a warning, not a hard error");
        assert!(result.dump.starts_with("badT.pdf: linearization data:\n\n"));
        assert!(result.dump.contains("\nPage Offsets Hint Table\n\n"));
        assert_eq!(result.warnings.len(), 1);
        assert!(
            result.warnings[0].contains("space before first xref item (/T) mismatch")
                && result.warnings[0].contains("file = 0"),
            "unexpected warning: {:?}",
            result.warnings
        );
    }

    #[test]
    fn hint_stream_s_offset_out_of_bounds_is_a_warning_with_no_dump() {
        // qpdf's readLinearizationData bounds-checks /S (the shared-object
        // hint table offset) against the decoded hint stream size and
        // throws damagedPDF("linearization hint table", "/S (shared
        // object) offset is out of bounds") when it does not fit
        // (QPDF_linearization.cc:270-277). That throw is caught by
        // showLinearizationData's single try/catch (`:837-846`) and
        // becomes one bare warning with no dump, not a hard error.
        //
        // Verified against `/usr/bin/qpdf` 11.9.0 on this same fixture
        // shape: it prints exactly one `WARNING: ...: ... /S (shared
        // object) offset is out of bounds` line, no dump, and exits 3.
        // The warning keeps qpdf's damagedPDF category and source offset,
        // including the distinction between the hint table and hint stream
        // failures.
        let mut bytes = linearized_bytes();
        let s_pos = bytes.windows(3).position(|w| w == b"/S ").expect("hint /S");
        let dstart = s_pos + 3;
        let dend = dstart
            + bytes[dstart..]
                .iter()
                .position(|&b| !b.is_ascii_digit())
                .expect("/S numeric value");
        // Same width (2 digits) as the original value, so no downstream
        // byte offset shifts -- just large enough to exceed the decoded
        // hint stream's payload length.
        bytes[dstart..dend].copy_from_slice(b"99");

        let result = show_linearization_bytes_with_warnings(&bytes, "badS.pdf")
            .expect("an out-of-bounds /S is a qpdf warning, not a hard error");
        assert!(result.dump.is_empty());
        assert_eq!(
            result.warnings,
            ["badS.pdf (linearization hint table, offset 568): /S (shared object) offset is out of bounds".to_owned()]
        );
    }

    #[test]
    fn compat_hint_stream_s_offset_uses_qpdf_source_offset() {
        let mut bytes = compat_linearized_bytes();
        replace_numeric_value(&mut bytes, b"/S ", b"99");

        let result = show_linearization_bytes_with_warnings(&bytes, "compatS.pdf")
            .expect("an out-of-bounds /S is a qpdf warning");
        assert!(result.dump.is_empty());
        assert_eq!(
            result.warnings,
            ["compatS.pdf (linearization hint table, offset 660): /S (shared object) offset is out of bounds".to_owned()]
        );
    }

    #[test]
    fn hint_stream_header_failure_uses_qpdf_damage_context() {
        // qpdf's readObjectAtOffset throws damagedPDF(offset,
        // "expected n n obj") while the current object description is
        // "linearization hint stream" (QPDF.cc:1542-1636).
        let mut bytes = compat_linearized_bytes();
        replace_hint_offset(&mut bytes, b"999");

        let result = show_linearization_bytes_with_warnings(&bytes, "badH.pdf")
            .expect("a bad hint-stream header is a qpdf warning");
        assert!(result.dump.is_empty());
        assert_eq!(
            result.warnings,
            ["badH.pdf (linearization hint stream, offset 999): expected n n obj".to_owned()]
        );
    }

    #[test]
    fn non_stream_hint_object_uses_qpdf_dictionary_damage_context() {
        // qpdf's readHintStream reports a non-stream H object through
        // damagedPDF("linearization dictionary", "hint table is not a stream")
        // (QPDF_linearization.cc:286-294), with the parser's last source
        // offset supplied by the overload.
        let mut bytes = compat_linearized_bytes();
        replace_hint_offset(&mut bytes, b"533");

        let result = show_linearization_bytes_with_warnings(&bytes, "badHn.pdf")
            .expect("a non-stream hint object is a qpdf warning");
        assert!(result.dump.is_empty());
        assert_eq!(
            result.warnings,
            [
                "badHn.pdf (linearization dictionary, offset 594): hint table is not a stream"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn non_stream_hint_short_trailing_token_uses_qpdf_read_offset() {
        let mut bytes = compat_linearized_bytes();
        let s_pos = bytes.windows(3).position(|w| w == b"/S ").expect("hint /S");
        let stream_pos = bytes[s_pos..]
            .windows(7)
            .position(|w| w == b"stream\n")
            .map(|position| s_pos + position)
            .expect("hint stream keyword");
        // Keep the file length and all later offsets fixed while changing the
        // trailing token from six bytes to three bytes plus whitespace.
        bytes[stream_pos..stream_pos + 7].copy_from_slice(b"bad   \n");

        let result = show_linearization_bytes_with_warnings(&bytes, "short-trailing.pdf")
            .expect("a non-endobj trailing token is a qpdf warning");
        assert_eq!(
            result.warnings,
            [
                "short-trailing.pdf (linearization dictionary, offset 660): hint table is not a stream"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn cached_non_stream_hint_short_trailing_token_uses_token_start() {
        let mut bytes = compat_linearized_bytes();
        let s_pos = bytes.windows(3).position(|w| w == b"/S ").expect("hint /S");
        let stream_pos = bytes[s_pos..]
            .windows(7)
            .position(|w| w == b"stream\n")
            .map(|position| s_pos + position)
            .expect("hint stream keyword");
        bytes[stream_pos..stream_pos + 7].copy_from_slice(b"bad   \n");

        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("fixture should open");
        let (_, first_offset) = pdf
            .resolve_at_offset_with_damage_offset(601, ObjectRef::new(5, 0))
            .expect("first offset read should succeed");
        assert_eq!(first_offset, Some(660));
        let error = load_hint_stream_with_damage(&mut pdf, &bytes, 601, 118)
            .expect_err("a cached non-stream hint object is damaged");
        match error {
            HintStreamLoadError::Damage(damage) => assert_eq!(damage.offset, 653),
            // cov:ignore-start: this fixture intentionally exercises only the damage variant
            HintStreamLoadError::Core(error) => {
                panic!("unexpected core error: {error}")
            } // cov:ignore-end
        }
    }

    #[test]
    fn uncached_non_stream_hint_short_trailing_token_uses_end_after_space() {
        let mut bytes = compat_linearized_bytes();
        // Object 7 is not resolved by the linearization parameter preflight;
        // qpdf therefore reports the first non-whitespace byte after its
        // terminator rather than the terminator token's start.
        replace_hint_offset(&mut bytes, b"908");

        let result = show_linearization_bytes_with_warnings(&bytes, "uncached-trailing.pdf")
            .expect("an uncached non-stream hint object is a qpdf warning");
        assert_eq!(
            result.warnings,
            [
                "uncached-trailing.pdf (linearization dictionary, offset 939): hint table is not a stream"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn missing_required_key_is_malformed() {
        // Rename /N to /Z so qpdf's readLinearizationData warning path is
        // exercised: --show-linearization emits no dump and exits 3.
        let mut bytes = linearized_bytes();
        let pos = bytes.windows(3).position(|w| w == b"/N ").expect("/N");
        bytes[pos + 1] = b'Z';
        let result = show_linearization_bytes_with_warnings(&bytes, "noN.pdf")
            .expect("missing /N is a qpdf warning");
        assert!(result.dump.is_empty());
        assert_eq!(
            result.warnings,
            ["noN.pdf (linearization dictionary, offset 23): some keys in linearization dictionary are of the wrong type".to_owned()]
        );
    }

    #[test]
    fn decode_failure_is_malformed() {
        // Flip a byte inside the FlateDecode hint stream payload so the
        // deflate stream fails to decode. The hint stream is the object
        // pointed at by /H[0]; its compressed payload follows `stream\n`.
        // qpdf's `readHintStream` throw for this
        // (`QPDF_linearization.cc:284-322`) is caught by
        // `showLinearizationData`'s single try/catch (`:837-846`) and
        // becomes one bare warning with no dump, not a hard error.
        let mut bytes = linearized_bytes();
        // Find the hint stream dict (it carries `/S `) and corrupt a payload byte.
        let s_pos = bytes.windows(3).position(|w| w == b"/S ").expect("hint /S");
        let stream_kw = bytes[s_pos..]
            .windows(7)
            .position(|w| w == b"stream\n")
            .map(|p| s_pos + p + 7)
            .expect("stream keyword after hint dict");
        // Corrupt a byte a few into the deflate payload (past the zlib header).
        bytes[stream_kw + 4] ^= 0xFF;
        let result = show_linearization_bytes_with_warnings(&bytes, "badhint.pdf")
            .expect("a corrupted hint stream is a qpdf warning, not a hard error");
        assert!(result.dump.is_empty());
        // The exact warning text depends on which DEFLATE backend rejects
        // the corrupted bytes (see the identical note on
        // show_reports_a_corrupted_overflow_hint_stream_as_malformed);
        // assert the shape, not the wording.
        assert_eq!(result.warnings.len(), 1, "warnings: {:?}", result.warnings);
    }

    #[test]
    fn show_consumer_production_uses_the_canonical_object_handle_route() {
        let source = include_str!("show.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("show.rs has a test module")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "resolve_borrowed(",
            "decode_stream_data(",
            "page_refs(",
            "Object::",
        ] {
            assert!(
                !production.contains(forbidden),
                "show.rs production must not use legacy {forbidden} route"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Helper-level error arms via hand-built dictionaries (no byte fragility).
    // -----------------------------------------------------------------------

    fn full_param_dict() -> ObjectHandle {
        ObjectHandle::dictionary(vec![
            (b"/O".to_vec(), ObjectHandle::integer(8)),
            (b"/E".to_vec(), ObjectHandle::integer(1212)),
            (b"/N".to_vec(), ObjectHandle::integer(2)),
            (b"/T".to_vec(), ObjectHandle::integer(1885)),
            (
                b"/H".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::integer(601), ObjectHandle::integer(128)]),
            ),
        ])
    }

    #[test]
    fn read_lin_parameters_happy() {
        let d = full_param_dict();
        d.replace_key(b"/P", ObjectHandle::integer(0)).unwrap();
        let p = read_lin_parameters(&d, 2103).unwrap();
        assert_eq!(p.file_size, 2103);
        assert_eq!(p.first_page_object, 8);
        assert_eq!(p.first_page_end, 1212);
        assert_eq!(p.npages, 2);
        assert_eq!(p.xref_zero_offset, 1885);
        assert_eq!(p.first_page, 0);
        assert_eq!(p.h_offset, 601);
        assert_eq!(p.h_length, 128);
    }

    #[test]
    fn read_lin_parameters_default_first_page_when_p_absent() {
        let p = read_lin_parameters(&full_param_dict(), 2103).unwrap();
        assert_eq!(p.first_page, 0, "/P absent defaults to 0");
    }

    #[test]
    fn read_lin_parameters_p_wrong_type_is_malformed() {
        let d = full_param_dict();
        d.replace_key(b"/P", ObjectHandle::name(b"oops".to_vec()))
            .unwrap();
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_h_too_short_is_malformed() {
        let d = full_param_dict();
        d.replace_key(b"/H", ObjectHandle::array(vec![ObjectHandle::integer(601)]))
            .unwrap();
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_rejects_non_integer_deeper_parameter() {
        let d = full_param_dict();
        d.replace_key(b"/N", ObjectHandle::real(2.0)).unwrap();
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_rejects_three_item_hint_array() {
        let d = full_param_dict();
        d.replace_key(
            b"/H",
            ObjectHandle::array(vec![
                ObjectHandle::integer(601),
                ObjectHandle::integer(128),
                ObjectHandle::integer(0),
            ]),
        )
        .unwrap();
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_accepts_the_four_item_overflow_hint_array() {
        let d = full_param_dict();
        d.replace_key(
            b"/H",
            ObjectHandle::array(vec![
                ObjectHandle::integer(601),
                ObjectHandle::integer(128),
                ObjectHandle::integer(1700),
                ObjectHandle::integer(64),
            ]),
        )
        .unwrap();
        let p = read_lin_parameters(&d, 2103).expect("qpdf accepts a four-item /H array");
        assert_eq!(p.h_offset, 601);
        assert_eq!(p.h_length, 128);
    }

    #[test]
    fn read_lin_parameters_h_not_array_is_malformed() {
        let d = full_param_dict();
        d.replace_key(b"/H", ObjectHandle::integer(5)).unwrap();
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_h0_wrong_type_is_malformed() {
        let d = full_param_dict();
        d.replace_key(
            b"/H",
            ObjectHandle::array(vec![
                ObjectHandle::name(b"x".to_vec()),
                ObjectHandle::integer(128),
            ]),
        )
        .unwrap();
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_h1_wrong_type_is_malformed() {
        let d = full_param_dict();
        d.replace_key(
            b"/H",
            ObjectHandle::array(vec![
                ObjectHandle::integer(601),
                ObjectHandle::name(b"x".to_vec()),
            ]),
        )
        .unwrap();
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_hint_offsets_happy_and_outline() {
        let d = ObjectHandle::dictionary(vec![
            (b"/S".to_vec(), ObjectHandle::integer(36)),
            (b"/O".to_vec(), ObjectHandle::integer(72)),
        ]);
        assert_eq!(read_hint_offsets(&d).unwrap(), (36, Some(72)));

        // No /O → None.
        let d2 = ObjectHandle::dictionary(vec![(b"/S".to_vec(), ObjectHandle::integer(36))]);
        assert_eq!(read_hint_offsets(&d2).unwrap(), (36, None));
    }

    #[test]
    fn read_hint_offsets_missing_s_is_malformed() {
        let d = ObjectHandle::dictionary(Vec::new());
        assert!(matches!(
            read_hint_offsets(&d),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_hint_offsets_negative_outline_is_malformed() {
        let d = ObjectHandle::dictionary(vec![
            (b"/S".to_vec(), ObjectHandle::integer(36)),
            (b"/O".to_vec(), ObjectHandle::integer(-1)),
        ]);
        assert!(matches!(
            read_hint_offsets(&d),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    // -----------------------------------------------------------------------
    // parse_obj_header negative branches.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_obj_header_negative_branches() {
        assert_eq!(parse_obj_header(b""), None); // no digits
        assert_eq!(parse_obj_header(b"7"), None); // number, then EOF (no ws)
        assert_eq!(parse_obj_header(b"7 "), None); // number + ws, then EOF (no gen)
        assert_eq!(parse_obj_header(b"7 0"), None); // gen, then EOF (no ws)
        assert_eq!(parse_obj_header(b"7 0 "), None); // ws, then EOF (no obj)
        assert_eq!(parse_obj_header(b"7 0 xyz"), None); // not the obj keyword
        assert_eq!(parse_obj_header(b"7 x obj"), None); // gen not digits
                                                        // Leading whitespace must be skipped before the object number.
        assert_eq!(parse_obj_header(b"  7 0 obj"), Some(ObjectRef::new(7, 0)));
    }

    // -----------------------------------------------------------------------
    // is_linearized: qpdf isLinearized test (numeric /Linearized + /L == len).
    // -----------------------------------------------------------------------

    #[test]
    fn is_linearized_accepts_integer_and_real_one() {
        let d = ObjectHandle::dictionary(vec![(b"/Linearized".to_vec(), ObjectHandle::integer(1))]);
        assert!(is_linearized(&d, 100).unwrap());

        let d2 = ObjectHandle::dictionary(vec![(b"/Linearized".to_vec(), ObjectHandle::real(1.0))]);
        assert!(is_linearized(&d2, 100).unwrap());
    }

    #[test]
    fn is_linearized_rejects_missing_or_wrong_linearized() {
        // Missing key.
        assert!(!is_linearized(&ObjectHandle::dictionary(Vec::new()), 100).unwrap());
        // Wrong value.
        let d = ObjectHandle::dictionary(vec![(b"/Linearized".to_vec(), ObjectHandle::integer(0))]);
        assert!(!is_linearized(&d, 100).unwrap());
    }

    #[test]
    fn is_linearized_rejects_l_mismatch() {
        let d = ObjectHandle::dictionary(vec![
            (b"/Linearized".to_vec(), ObjectHandle::integer(1)),
            (b"/L".to_vec(), ObjectHandle::integer(999)),
        ]);
        assert!(!is_linearized(&d, 100).unwrap());
        // Matching /L is accepted.
        d.replace_key(b"/L", ObjectHandle::integer(100)).unwrap();
        assert!(is_linearized(&d, 100).unwrap());
    }

    #[test]
    fn show_bytes_dict_without_linearized_is_not_linearized() {
        // First object is a dict but lacks /Linearized → hits the
        // `!is_linearized` branch in show_with_pdf.
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n\
             {off3:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        let out = show_linearization_bytes(&pdf, "plain.pdf").expect("not-linearized is Ok");
        assert_eq!(out, "plain.pdf is not linearized\n");
    }
}

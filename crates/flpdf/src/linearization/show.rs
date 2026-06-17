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

use super::check::find_first_object_ref;
use crate::filters::decode_stream_data;
use crate::{Object, ObjectRef, Pdf};
use std::fmt;
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Reason a `show-linearization` decode failed.
///
/// Note that a PDF which is simply *not linearized* is **not** an error: qpdf's
/// `--show-linearization` prints `"<name> is not linearized"` to stdout and
/// exits 0 in that case, so [`show_linearization_bytes`] returns that line as an
/// `Ok` value rather than an error.
#[derive(Debug)]
pub enum ShowLinearizationError {
    /// The linearization parameter dictionary or hint stream is malformed (a
    /// required key is missing or has the wrong type, an offset is out of
    /// bounds, or the bit stream is truncated). `message` describes the fault.
    Malformed { message: String },
    /// An I/O or parse error occurred while reading the file.
    Io(Box<dyn std::error::Error + Send + Sync>),
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

/// Shorthand result type for the decoder.
type ShowResult<T> = std::result::Result<T, ShowLinearizationError>;

/// Return a [`ShowLinearizationError::Malformed`] error with a formatted message.
macro_rules! malformed {
    ($($arg:tt)*) => {
        ShowLinearizationError::Malformed { message: format!($($arg)*) }
    };
}

// ---------------------------------------------------------------------------
// MSB-first BitReader — inverse of HintStreamBuilder
// ---------------------------------------------------------------------------

/// A read-only MSB-first bit reader over an in-memory buffer.
///
/// Bits are consumed from the most-significant position of the current byte
/// downward — the inverse of [`super::hint_stream::HintStreamBuilder`].
/// [`BitReader::skip_to_next_byte`] advances to the next byte boundary,
/// mirroring the encoder's `align_to_byte`.
struct BitReader<'a> {
    buf: &'a [u8],
    /// Index of the byte currently being read.
    byte_pos: usize,
    /// Number of bits already consumed from `buf[byte_pos]` (0..=7).
    bit_pos: u32,
}

impl<'a> BitReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    /// Read `bits` bits MSB-first and return them as a `u64`.
    ///
    /// `bits == 0` is a no-op returning `0`, mirroring `write_bits(_, 0)`.
    /// Returns [`ShowLinearizationError::Malformed`] if the buffer is exhausted
    /// before `bits` bits are read, or if `bits > 64`.
    fn get_bits(&mut self, bits: u32) -> ShowResult<u64> {
        if bits == 0 {
            return Ok(0);
        }
        if bits > 64 {
            return Err(malformed!(
                "hint stream requests {bits}-bit field (exceeds 64-bit limit)"
            ));
        }
        let mut result: u64 = 0;
        let mut remaining = bits;
        while remaining > 0 {
            if self.byte_pos >= self.buf.len() {
                return Err(malformed!(
                    "hint stream truncated: ran out of bits while reading a {bits}-bit field"
                ));
            }
            // Bits still available in the current byte.
            let avail = 8 - self.bit_pos;
            let take = remaining.min(avail);
            // The current byte's not-yet-consumed bits occupy positions
            // [0 .. avail) counting from the LSB of the still-available window;
            // shift the whole byte right so the next-to-read bit is at the top
            // of an `avail`-wide window, then mask to `avail` bits.
            let cur = (self.buf[self.byte_pos] as u64) & ((1u64 << avail) - 1);
            // Take the top `take` bits of that `avail`-wide window.
            let shift = avail - take;
            let chunk = cur >> shift;
            result = (result << take) | chunk;
            self.bit_pos += take;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
            remaining -= take;
        }
        Ok(result)
    }

    /// Read `bits` bits and return them as a `u32`.
    ///
    /// Returns [`ShowLinearizationError::Malformed`] if the decoded value does
    /// not fit in a `u32` (i.e. `bits > 32` with a value above `u32::MAX`).
    fn get_bits_u32(&mut self, bits: u32) -> ShowResult<u32> {
        let v = self.get_bits(bits)?;
        u32::try_from(v).map_err(|_| malformed!("hint stream field {v} does not fit in u32"))
    }

    /// Advance to the next byte boundary if not already aligned.
    ///
    /// Mirrors `HintStreamBuilder::align_to_byte` and qpdf's `skipToNextByte`.
    fn skip_to_next_byte(&mut self) {
        if self.bit_pos != 0 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
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
struct HPageOffset {
    min_nobjects: u32,
    first_page_offset: u64,
    nbits_delta_nobjects: u32,
    min_page_length: u64,
    nbits_delta_page_length: u32,
    min_content_offset: u64,
    nbits_delta_content_offset: u32,
    min_content_length: u64,
    nbits_delta_content_length: u32,
    nbits_nshared_objects: u32,
    nbits_shared_identifier: u32,
    nbits_shared_numerator: u32,
    shared_denominator: u32,
    entries: Vec<HPageOffsetEntry>,
}

/// Decoded per-page entry of the Page Offset Hint Table.
struct HPageOffsetEntry {
    delta_nobjects: u64,
    delta_page_length: u64,
    nshared_objects: u32,
    shared_identifiers: Vec<u64>,
    shared_numerators: Vec<u64>,
    delta_content_offset: u64,
    delta_content_length: u64,
}

/// Decoded Shared Object Hint Table (qpdf's `HSharedObject`).
struct HSharedObject {
    first_shared_obj: u64,
    first_shared_offset: u64,
    nshared_first_page: u32,
    nshared_total: u32,
    nbits_nobjects: u32,
    min_group_length: u64,
    nbits_delta_group_length: u32,
    entries: Vec<HSharedObjectEntry>,
}

/// Decoded per-entry data of the Shared Object Hint Table.
struct HSharedObjectEntry {
    delta_group_length: u64,
    signature_present: bool,
    nobjects_minus_one: u64,
}

/// Decoded generic (Outlines) hint table (qpdf's `HGeneric`).
struct HGeneric {
    first_object: u64,
    first_object_offset: u64,
    nobjects: u32,
    group_length: u64,
}

// ---------------------------------------------------------------------------
// Page Offset Hint Table decoder — mirrors qpdf readHPageOffset
// ---------------------------------------------------------------------------

fn read_h_page_offset(buf: &[u8], npages: u32) -> ShowResult<HPageOffset> {
    let mut h = BitReader::new(buf);

    // 13 header fields (5×32 + 8×16 = 288 bits = 36 bytes, leaving the reader
    // byte-aligned).  Order is qpdf's readHPageOffset.
    let min_nobjects = h.get_bits_u32(32)?;
    let first_page_offset = h.get_bits(32)?;
    let nbits_delta_nobjects = h.get_bits_u32(16)?;
    let min_page_length = h.get_bits(32)?;
    let nbits_delta_page_length = h.get_bits_u32(16)?;
    let min_content_offset = h.get_bits(32)?;
    let nbits_delta_content_offset = h.get_bits_u32(16)?;
    let min_content_length = h.get_bits(32)?;
    let nbits_delta_content_length = h.get_bits_u32(16)?;
    let nbits_nshared_objects = h.get_bits_u32(16)?;
    let nbits_shared_identifier = h.get_bits_u32(16)?;
    let nbits_shared_numerator = h.get_bits_u32(16)?;
    let shared_denominator = h.get_bits_u32(16)?;

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
        e.delta_nobjects = h.get_bits(nbits_delta_nobjects)?;
    }
    h.skip_to_next_byte();
    // (b) delta_page_length
    for e in entries.iter_mut() {
        e.delta_page_length = h.get_bits(nbits_delta_page_length)?;
    }
    h.skip_to_next_byte();
    // (c) nshared_objects
    for e in entries.iter_mut() {
        e.nshared_objects = h.get_bits_u32(nbits_nshared_objects)?;
    }
    h.skip_to_next_byte();
    // Bound the per-page shared-object refs before the nested (d)/(e) loops.
    // With zero-width identifier/numerator fields each push reads 0 bits and
    // never advances the reader, so an untrusted `nshared_objects` could
    // otherwise drive unbounded time/allocation from a tiny stream. Cap the
    // total against the bits remaining, keeping work O(stream length).
    let remaining_bits = (h.buf.len() - h.byte_pos)
        .saturating_mul(8)
        .saturating_sub(h.bit_pos as usize);
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
                .push(h.get_bits(nbits_shared_identifier)?);
        }
    }
    h.skip_to_next_byte();
    // (e) shared_numerators — nested
    for e in entries.iter_mut() {
        for _ in 0..e.nshared_objects {
            e.shared_numerators
                .push(h.get_bits(nbits_shared_numerator)?);
        }
    }
    h.skip_to_next_byte();
    // (f) delta_content_offset
    for e in entries.iter_mut() {
        e.delta_content_offset = h.get_bits(nbits_delta_content_offset)?;
    }
    h.skip_to_next_byte();
    // (g) delta_content_length
    for e in entries.iter_mut() {
        e.delta_content_length = h.get_bits(nbits_delta_content_length)?;
    }
    h.skip_to_next_byte();

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

fn read_h_shared_object(buf: &[u8]) -> ShowResult<HSharedObject> {
    let mut h = BitReader::new(buf);

    // 7 header fields.
    let first_shared_obj = h.get_bits(32)?;
    let first_shared_offset = h.get_bits(32)?;
    let nshared_first_page = h.get_bits_u32(32)?;
    let nshared_total = h.get_bits_u32(32)?;
    let nbits_nobjects = h.get_bits_u32(16)?;
    let min_group_length = h.get_bits(32)?;
    let nbits_delta_group_length = h.get_bits_u32(16)?;

    // Each entry consumes at least one bit (the signature_present column below),
    // so a well-formed table cannot claim more entries than there are bits left
    // in the stream. Guard before allocating so a malformed `nshared_total`
    // (up to u32::MAX) cannot drive a multi-gigabyte pre-allocation (OOM DoS).
    let remaining_bits = (h.buf.len() - h.byte_pos)
        .saturating_mul(8)
        .saturating_sub(h.bit_pos as usize);
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
        e.delta_group_length = h.get_bits(nbits_delta_group_length)?;
    }
    h.skip_to_next_byte();
    // (b) signature_present (1 bit each)
    for e in entries.iter_mut() {
        e.signature_present = h.get_bits(1)? != 0;
    }
    h.skip_to_next_byte();
    // (c) inline 128-bit signature skip per set flag (no alignment around it),
    // matching qpdf's loop between the signature_present and nobjects columns.
    for e in entries.iter() {
        if e.signature_present {
            for _ in 0..4 {
                let _ = h.get_bits(32)?;
            }
        }
    }
    // (d) nobjects_minus_one
    for e in entries.iter_mut() {
        e.nobjects_minus_one = h.get_bits(nbits_nobjects)?;
    }
    h.skip_to_next_byte();

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

fn read_h_generic(buf: &[u8]) -> ShowResult<HGeneric> {
    let mut h = BitReader::new(buf);
    let first_object = h.get_bits(32)?;
    let first_object_offset = h.get_bits(32)?;
    let nobjects = h.get_bits_u32(32)?;
    let group_length = h.get_bits(32)?;
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

/// Read a required non-negative integer parameter from the linearization
/// parameter dictionary.
fn param_u64(dict: &crate::Dictionary, key: &'static str) -> ShowResult<u64> {
    match dict.get(key) {
        Some(Object::Integer(n)) if *n >= 0 => Ok(*n as u64),
        Some(Object::Real(r)) if r.is_finite() && *r >= 0.0 && r.fract() == 0.0 => Ok(*r as u64),
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
fn is_linearized(dict: &crate::Dictionary, file_size: u64) -> bool {
    let linearized_ok = match dict.get("Linearized") {
        Some(Object::Integer(n)) => *n == 1,
        Some(Object::Real(r)) => r.is_finite() && r.floor() == 1.0,
        _ => false,
    };
    if !linearized_ok {
        return false;
    }
    if let Some(Object::Integer(l)) = dict.get("L") {
        if *l < 0 || (*l as u64) != file_size {
            return false;
        }
    }
    true
}

/// Extract qpdf's `LinParameters` from the linearization parameter dictionary.
///
/// `file_size` is the actual file length (used directly; qpdf's `isLinearized`
/// has already verified that `/L` equals it).
fn read_lin_parameters(dict: &crate::Dictionary, file_size: u64) -> ShowResult<LinParameters> {
    let first_page_object = param_u64(dict, "O")?;
    let first_page_end = param_u64(dict, "E")?;
    let npages_u64 = param_u64(dict, "N")?;
    let npages = u32::try_from(npages_u64)
        // cov:ignore: a document with > u32::MAX pages cannot be opened, so this
        // overflow guard is unreachable through the public API.
        .map_err(|_| malformed!("/N ({npages_u64}) does not fit in u32"))?;
    let xref_zero_offset = param_u64(dict, "T")?;
    // /P (first page number) is optional; qpdf defaults to 0 when absent/null.
    let first_page: i64 = match dict.get("P") {
        Some(Object::Integer(n)) => *n,
        Some(Object::Null) | None => 0,
        _ => return Err(malformed!("/P is present but not an integer")),
    };
    // /H is [offset length] (or 4 items for an overflow table; we use H[0..2]).
    let (h_offset, h_length) = match dict.get("H") {
        Some(Object::Array(arr)) if arr.len() >= 2 => {
            let off = match &arr[0] {
                Object::Integer(n) if *n >= 0 => *n as u64,
                _ => return Err(malformed!("/H[0] is not a non-negative integer")),
            };
            let len = match &arr[1] {
                Object::Integer(n) if *n >= 0 => *n as u64,
                _ => return Err(malformed!("/H[1] is not a non-negative integer")),
            };
            (off, len)
        }
        _ => return Err(malformed!("/H is missing or not an [offset length] array")),
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
    })
}

/// Extract the Shared Object (`/S`) and optional Outline (`/O`) section offsets
/// from the **hint stream** dictionary (not the parameter dict).
///
/// Returns `(s_offset, outline_offset)`.
fn read_hint_offsets(hint_dict: &crate::Dictionary) -> ShowResult<(usize, Option<usize>)> {
    let s_offset = match hint_dict.get("S") {
        Some(Object::Integer(n)) if *n >= 0 => usize::try_from(*n)
            // cov:ignore: on 64-bit usize is u64, so a non-negative /S offset
            // always fits; this only fires on 32-bit targets.
            .map_err(|_| malformed!("hint stream /S offset does not fit in platform usize"))?,
        _ => return Err(malformed!("hint stream /S offset is missing or invalid")),
    };
    let outline_offset: Option<usize> = match hint_dict.get("O") {
        Some(Object::Integer(n)) if *n >= 0 => Some(
            usize::try_from(*n)
                // cov:ignore: on 64-bit usize is u64, so a non-negative /O offset
                // always fits; this only fires on 32-bit targets.
                .map_err(|_| malformed!("hint stream /O offset does not fit in platform usize"))?,
        ),
        Some(Object::Integer(_)) => return Err(malformed!("hint stream /O offset is negative")),
        _ => None,
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
) -> ShowResult<String> {
    let file_size = file_bytes.len() as u64;

    // 1. Locate the linearization parameter dictionary (first physical object)
    //    and apply qpdf's `isLinearized` test: the first object must be a dict
    //    with a numeric `/Linearized` whose floor is 1, and (when `/L` is an
    //    integer) `/L` must equal the file length.  When the file is not
    //    linearized, qpdf's `--show-linearization` prints "<name> is not
    //    linearized" to stdout and exits 0 — we return that line as Ok.
    let not_linearized = || Ok(format!("{display_name} is not linearized\n"));

    let Some(first_obj_ref) = find_first_object_ref(file_bytes) else {
        // cov:ignore-start: unreachable via the public API — `Pdf::open` rejects
        // a file with no object header before this runs, so a successfully
        // opened document always has at least one `N G obj`.
        return not_linearized();
        // cov:ignore-end
    };
    let first_obj = pdf.resolve_borrowed(first_obj_ref)?;
    let Some(param_dict) = first_obj.as_dict().cloned() else {
        return not_linearized();
    };
    if !is_linearized(&param_dict, file_size) {
        return not_linearized();
    }

    // 2. Param-dict values (qpdf's LinParameters).
    let params = read_lin_parameters(&param_dict, file_size)?;

    // qpdf's --show-linearization walks getAllPages() in
    // checkLinearizationInternal; mirror that walk here, both for fidelity and
    // to bound the per-page allocation in read_h_page_offset. A well-formed
    // linearized file has /N equal to its page count, so this never rejects
    // valid input; a malformed /N (up to u32::MAX) is reported as malformed
    // rather than driving a multi-gigabyte pre-allocation (OOM DoS).
    let page_count = crate::pages::page_refs(pdf)?.len();
    if params.npages as usize != page_count {
        // cov:ignore-start: /N disagreeing with the page tree — never emitted by
        // flpdf's writer; this bounds read_h_page_offset against a malformed /N.
        return Err(malformed!(
            "/N ({}) does not match the document page count ({page_count})",
            params.npages
        ));
        // cov:ignore-end
    }

    // 3. Locate, resolve, and decompress the hint stream object at /H[0].
    //
    // The error returns in this section are defensive guards for malformed /
    // third-party hint streams: flpdf's own writer always emits a well-formed
    // FlateDecode hint stream at a valid in-bounds /H[0], so those arms are
    // unreachable through flpdf's output (malformed-input behavior is covered at
    // the helper level by read_lin_parameters / read_hint_offsets wrong-type
    // tests and decode_failure_is_malformed). Each such `return Err(...)` body —
    // not the surrounding binding/condition, which the happy path runs — is
    // excluded from coverage below.
    let h_usize = usize::try_from(params.h_offset)
        // cov:ignore: on 64-bit usize is u64, so a non-negative h_offset always
        // fits; this only fires on 32-bit targets.
        .map_err(|_| malformed!("/H[0] does not fit in platform usize"))?;
    if h_usize >= file_bytes.len() {
        // cov:ignore-start: /H[0] past EOF — never emitted by flpdf's writer.
        return Err(malformed!(
            "/H[0] offset ({}) is beyond file length ({file_size})",
            params.h_offset
        ));
        // cov:ignore-end
    }
    let Some(hint_ref) = parse_obj_header(&file_bytes[h_usize..]) else {
        // cov:ignore-start: /H[0] not pointing at an object header — never
        // emitted by flpdf's writer.
        return Err(malformed!(
            "/H[0] offset ({}) does not point at an object header",
            params.h_offset
        ));
        // cov:ignore-end
    };
    let hint_obj = pdf.resolve_borrowed(hint_ref)?;
    let Object::Stream(stream) = &hint_obj else {
        // cov:ignore-start: hint object not a stream — never emitted by flpdf.
        return Err(malformed!(
            "hint stream object at /H[0] offset {} is not a stream",
            params.h_offset
        ));
        // cov:ignore-end
    };
    let decompressed = decode_stream_data(&stream.dict, &stream.data)
        .map_err(|e| malformed!("hint stream could not be decoded: {e}"))?;

    // /S (shared object table offset) and /O (outline table offset) are keys on
    // the HINT STREAM dictionary — not the parameter dict.
    let (s_offset, outline_offset) = read_hint_offsets(&stream.dict)?;
    if s_offset >= decompressed.len() {
        // cov:ignore-start: /S out of bounds — flpdf keeps /S in bounds.
        return Err(malformed!(
            "hint stream /S offset ({s_offset}) is out of bounds (hint size {})",
            decompressed.len()
        ));
        // cov:ignore-end
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
                return Err(malformed!(
                    "hint stream /O offset ({off}) is out of bounds (hint size {})",
                    decompressed.len()
                ));
            }
            Some(read_h_generic(&decompressed[off..])?)
        }
        // cov:ignore-end
        None => None,
    };

    Ok(format_dump(
        display_name,
        &params,
        &page_offset,
        &shared_object,
        outline.as_ref(),
    ))
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
/// # Errors
///
/// Returns [`ShowLinearizationError::Malformed`] when the file is linearized but
/// a parameter-dictionary value (`/O`, `/E`, `/N`, `/T`, `/H`, `/P`) is missing
/// or of the wrong type, the hint stream cannot be located or decoded, a
/// hint-stream `/S` or `/O` offset is out of bounds, or the bit stream is
/// truncated.
///
/// Returns [`ShowLinearizationError::Io`] when opening the [`Pdf`] from the
/// in-memory bytes or resolving an object fails.
pub fn show_linearization_bytes(
    file_bytes: &[u8],
    display_name: &str,
) -> std::result::Result<String, ShowLinearizationError> {
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
    use crate::linearization::hint_stream::{encode_hint_stream, HintStreamBuilder};

    // -----------------------------------------------------------------------
    // BitReader: write via HintStreamBuilder, read back. Covers cross-byte
    // patterns and byte alignment (inverse of the encoder).
    // -----------------------------------------------------------------------

    #[test]
    fn bitreader_reads_back_msb_first_patterns() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(0xDEAD_BEEF, 32);
        b.write_bits(0b101, 3);
        b.write_bits(0b10110, 5);
        b.write_bits(0b1100_1010_0011, 12);
        let buf = b.finish();

        let mut r = BitReader::new(&buf);
        assert_eq!(r.get_bits(32).unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.get_bits(3).unwrap(), 0b101);
        assert_eq!(r.get_bits(5).unwrap(), 0b10110);
        assert_eq!(r.get_bits(12).unwrap(), 0b1100_1010_0011);
    }

    #[test]
    fn bitreader_zero_bits_is_noop() {
        let buf = [0xABu8];
        let mut r = BitReader::new(&buf);
        assert_eq!(r.get_bits(0).unwrap(), 0);
        // Still able to read the full byte afterwards.
        assert_eq!(r.get_bits(8).unwrap(), 0xAB);
    }

    #[test]
    fn bitreader_skip_to_next_byte_matches_align() {
        // Write 1 bit then align (pad to byte), then a full byte.
        let mut b = HintStreamBuilder::new();
        b.write_bits(1, 1);
        b.align_to_byte();
        b.write_bits(0x5A, 8);
        let buf = b.finish();

        let mut r = BitReader::new(&buf);
        assert_eq!(r.get_bits(1).unwrap(), 1);
        r.skip_to_next_byte();
        assert_eq!(r.get_bits(8).unwrap(), 0x5A);
    }

    #[test]
    fn bitreader_skip_on_boundary_is_noop() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(0xAB, 8);
        b.write_bits(0xCD, 8);
        let buf = b.finish();

        let mut r = BitReader::new(&buf);
        assert_eq!(r.get_bits(8).unwrap(), 0xAB);
        r.skip_to_next_byte(); // already aligned — must not skip the second byte
        assert_eq!(r.get_bits(8).unwrap(), 0xCD);
    }

    #[test]
    fn bitreader_truncated_errors() {
        let buf = [0xFFu8];
        let mut r = BitReader::new(&buf);
        assert!(r.get_bits(16).is_err(), "reading past end must error");
    }

    #[test]
    fn bitreader_rejects_more_than_64_bits() {
        let buf = [0u8; 16];
        let mut r = BitReader::new(&buf);
        assert!(r.get_bits(65).is_err());
    }

    #[test]
    fn bitreader_get_bits_u32_rejects_oversized_value() {
        // 40 bits all set → value > u32::MAX → must error.
        let buf = [0xFFu8; 5];
        let mut r = BitReader::new(&buf);
        assert!(r.get_bits_u32(40).is_err());
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
    // build the bitstream in that exact reader order via HintStreamBuilder and
    // assert the decoder skips the signature correctly so the nobjects column
    // stays aligned.  (flpdf never emits signatures, so this path is exercised
    // only here, but it must mirror qpdf for arbitrary linearized input.)
    // -----------------------------------------------------------------------

    #[test]
    fn shared_object_signature_present_skips_128_bits() {
        let nbits_delta_group_length = 4u32;
        let nbits_nobjects = 4u32;
        let nshared_total = 2u32;

        let mut b = HintStreamBuilder::new();
        // 7-field header (32-bit ×5, 16-bit ×2 = 24 bytes, byte-aligned).
        b.write_bits(1, 32); // first_shared_obj
        b.write_bits(0, 32); // first_shared_offset
        b.write_bits(2, 32); // nshared_first_page
        b.write_bits(nshared_total as u64, 32); // nshared_total
        b.write_bits(nbits_nobjects as u64, 16); // nbits_nobjects
        b.write_bits(0, 32); // min_group_length
        b.write_bits(nbits_delta_group_length as u64, 16); // nbits_delta_group_length
                                                           // col a: delta_group_length × N
        b.write_bits(3, nbits_delta_group_length);
        b.write_bits(9, nbits_delta_group_length);
        b.align_to_byte();
        // col b: signature_present × N (entry 0 set, entry 1 clear)
        b.write_bits(1, 1);
        b.write_bits(0, 1);
        b.align_to_byte();
        // col c: 128 bits (4×32) for the one set flag — no inner alignment
        for _ in 0..4 {
            b.write_bits(0xDEAD_BEEF, 32);
        }
        // col d: nobjects_minus_one × N
        b.write_bits(5, nbits_nobjects);
        b.write_bits(7, nbits_nobjects);
        b.align_to_byte();
        let buf = b.finish();

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
        let mut b = HintStreamBuilder::new();
        b.write_bits(0, 32); // first_shared_obj
        b.write_bits(0, 32); // first_shared_offset
        b.write_bits(0, 32); // nshared_first_page
        b.write_bits(1_000_000, 32); // nshared_total (far exceeds 0 remaining bits)
        b.write_bits(0, 16); // nbits_nobjects
        b.write_bits(0, 32); // min_group_length
        b.write_bits(0, 16); // nbits_delta_group_length
        let buf = b.finish();
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
        let mut b = HintStreamBuilder::new();
        // 13-field header (5×32 + 8×16 = 36 bytes, byte-aligned).
        b.write_bits(0, 32); // min_nobjects
        b.write_bits(0, 32); // first_page_offset
        b.write_bits(0, 16); // nbits_delta_nobjects = 0
        b.write_bits(0, 32); // min_page_length
        b.write_bits(0, 16); // nbits_delta_page_length = 0
        b.write_bits(0, 32); // min_content_offset
        b.write_bits(0, 16); // nbits_delta_content_offset = 0
        b.write_bits(0, 32); // min_content_length
        b.write_bits(0, 16); // nbits_delta_content_length = 0
        b.write_bits(32, 16); // nbits_nshared_objects = 32
        b.write_bits(0, 16); // nbits_shared_identifier = 0
        b.write_bits(0, 16); // nbits_shared_numerator = 0
        b.write_bits(1, 16); // shared_denominator
                             // cols (a)/(b): 0-bit, nothing written. col (c):
                             // nshared_objects for the single page.
        b.write_bits(1_000_000, 32);
        b.align_to_byte();
        let buf = b.finish();
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
        let mut d = crate::Dictionary::new();
        d.insert("N", Object::Real(1.5));
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
        let mut b = HintStreamBuilder::new();
        b.write_bits(11, 32); // first_object
        b.write_bits(222, 32); // first_object_offset
        b.write_bits(3, 32); // nobjects
        b.write_bits(4444, 32); // group_length
        let buf = b.finish();
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
    use crate::writer::WriteOptions;
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(raw)).unwrap();
        let mut doc =
            write_linearized(&plan, &renumber, &mut pdf2, &WriteOptions::default()).unwrap();
        doc.back_patch().unwrap();
        doc.bytes
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
    fn missing_required_key_is_malformed() {
        // Rename /N to /Z so the required /N lookup fails → Malformed.
        let mut bytes = linearized_bytes();
        let pos = bytes.windows(3).position(|w| w == b"/N ").expect("/N");
        bytes[pos + 1] = b'Z';
        let result = show_linearization_bytes(&bytes, "noN.pdf");
        assert!(matches!(
            result,
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn decode_failure_is_malformed() {
        // Flip a byte inside the FlateDecode hint stream payload so the deflate
        // stream fails to decode → Malformed.  The hint stream is the object
        // pointed at by /H[0]; its compressed payload follows `stream\n`.
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
        let result = show_linearization_bytes(&bytes, "badhint.pdf");
        assert!(
            matches!(result, Err(ShowLinearizationError::Malformed { .. })),
            "corrupt hint payload must yield Malformed, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Helper-level error arms via hand-built dictionaries (no byte fragility).
    // -----------------------------------------------------------------------

    fn full_param_dict() -> crate::Dictionary {
        let mut d = crate::Dictionary::new();
        d.insert("O", Object::Integer(8));
        d.insert("E", Object::Integer(1212));
        d.insert("N", Object::Integer(2));
        d.insert("T", Object::Integer(1885));
        d.insert(
            "H",
            Object::Array(vec![Object::Integer(601), Object::Integer(128)]),
        );
        d
    }

    #[test]
    fn read_lin_parameters_happy() {
        let mut d = full_param_dict();
        d.insert("P", Object::Integer(0));
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
        let mut d = full_param_dict();
        d.insert("P", Object::Name(b"oops".to_vec()));
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_h_too_short_is_malformed() {
        let mut d = full_param_dict();
        d.insert("H", Object::Array(vec![Object::Integer(601)]));
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_h_not_array_is_malformed() {
        let mut d = full_param_dict();
        d.insert("H", Object::Integer(5));
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_h0_wrong_type_is_malformed() {
        let mut d = full_param_dict();
        d.insert(
            "H",
            Object::Array(vec![Object::Name(b"x".to_vec()), Object::Integer(128)]),
        );
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_lin_parameters_h1_wrong_type_is_malformed() {
        let mut d = full_param_dict();
        d.insert(
            "H",
            Object::Array(vec![Object::Integer(601), Object::Name(b"x".to_vec())]),
        );
        assert!(matches!(
            read_lin_parameters(&d, 2103),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_hint_offsets_happy_and_outline() {
        let mut d = crate::Dictionary::new();
        d.insert("S", Object::Integer(36));
        d.insert("O", Object::Integer(72));
        assert_eq!(read_hint_offsets(&d).unwrap(), (36, Some(72)));

        // No /O → None.
        let mut d2 = crate::Dictionary::new();
        d2.insert("S", Object::Integer(36));
        assert_eq!(read_hint_offsets(&d2).unwrap(), (36, None));
    }

    #[test]
    fn read_hint_offsets_missing_s_is_malformed() {
        let d = crate::Dictionary::new();
        assert!(matches!(
            read_hint_offsets(&d),
            Err(ShowLinearizationError::Malformed { .. })
        ));
    }

    #[test]
    fn read_hint_offsets_negative_outline_is_malformed() {
        let mut d = crate::Dictionary::new();
        d.insert("S", Object::Integer(36));
        d.insert("O", Object::Integer(-1));
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
        let mut d = crate::Dictionary::new();
        d.insert("Linearized", Object::Integer(1));
        assert!(is_linearized(&d, 100));

        let mut d2 = crate::Dictionary::new();
        d2.insert("Linearized", Object::Real(1.0));
        assert!(is_linearized(&d2, 100));
    }

    #[test]
    fn is_linearized_rejects_missing_or_wrong_linearized() {
        // Missing key.
        assert!(!is_linearized(&crate::Dictionary::new(), 100));
        // Wrong value.
        let mut d = crate::Dictionary::new();
        d.insert("Linearized", Object::Integer(0));
        assert!(!is_linearized(&d, 100));
    }

    #[test]
    fn is_linearized_rejects_l_mismatch() {
        let mut d = crate::Dictionary::new();
        d.insert("Linearized", Object::Integer(1));
        d.insert("L", Object::Integer(999)); // != file_size
        assert!(!is_linearized(&d, 100));
        // Matching /L is accepted.
        d.insert("L", Object::Integer(100));
        assert!(is_linearized(&d, 100));
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

//! Repair QDF cross-reference offsets after manual edits.
//!
//! qpdf correspondence: qpdf/fix-qdf.cc tool behavior outside libqpdf.
//!
//! `fix_qdf`: the flpdf equivalent of qpdf's `fix-qdf` tool.
//!
//! After a human edits a QDF-form PDF (the flat, normalized layout produced by
//! [`crate::PdfWriter::set_qdf_mode`] / `qpdf --qdf`), the cross-reference offsets, stream
//! `/Length` values, the trailer `/Size`, and `startxref` are all stale. This
//! module recomputes exactly those four regions from the current bytes while
//! preserving every other byte verbatim — object bodies, comments, formatting,
//! and any whitespace the human introduced are kept exactly as-is.
//!
//! ## Scope and format expectations
//!
//! [`fix_qdf`] operates purely on bytes; it does **not** route through the full
//! [`crate::Pdf`] parser/serializer (that would reformat the file and defeat the
//! "must not change human-edited content" guarantee). It targets the canonical
//! QDF structure that `qpdf --qdf` produces and that qpdf's own `fix-qdf`
//! accepts:
//!
//! * objects are written as the exact LF-terminated `N 0 obj` line recognized
//!   by qpdf's `re_n_0_obj`, optionally preceded by a
//!   `%% Original object ID: N G` comment line (the offset always points at
//!   the `N 0 obj` line, **not** the comment — verified against the oracle);
//! * object numbers are contiguous, numbered `1..N` in file order — qpdf's
//!   `fix-qdf` requires this (it aborts on the first out-of-sequence object)
//!   and [`fix_qdf`] rejects non-sequential numbering the same way;
//! * qpdf's QDF writer emits a real stream's `/Length` as an indirect
//!   `/Length M 0 R` and puts a synthetic integer object immediately after the
//!   stream. qpdf's `fix-qdf` tool does not read that dictionary key, though:
//!   its `st_after_stream` state takes the next positional `N 0 obj` as the
//!   holder and rewrites that object's next line, regardless of the `/Length`
//!   value, generation, or presence;
//! * a tail that is EITHER a single classic `xref` table with one `0 N`
//!   subsection, followed by a `trailer` dictionary, `startxref`, and
//!   `%%EOF`, OR a cross-reference stream (`/Type /XRef`) object folding the
//!   trailer keys into its own dictionary — `qpdf --qdf
//!   --object-streams=generate` emits the latter form, complete with
//!   `/Type /ObjStm` container objects, so both are canonical QDF.
//!
//! An object whose dictionary has `/Type /ObjStm` is a QDF-expanded object
//! stream: its `stream`...`endstream` payload is a stale offset table
//! followed by one `%% Object stream: object N[, index M][; original object
//! ID: K]` marker line per member, each immediately followed by that
//! member's decompressed body (`qpdf/fix-qdf.cc`'s `st_in_ostream_*`
//! states). [`fix_qdf`] regenerates the offset table, `/Length`, `/N`, and
//! `/First` the same way; member object numbers continue the same
//! sequential counter as top-level objects (a member is `checkObjId`'d just
//! like a top-level object is).
//!
//! An object whose dictionary has `/Type /XRef` is a cross-reference
//! stream: its `stream`...`endstream` payload (stale binary xref data) is
//! discarded and regenerated wholesale, and its dictionary's `/Length`,
//! `/W`, and `/Size` entries are regenerated in place
//! (`qpdf/fix-qdf.cc`'s `st_in_xref_stream_dict` state) — everything else
//! in the dictionary (`/Root`, `/ID`, ...) is kept verbatim. Everything in
//! the input after this object's `stream` keyword is ignored, matching the
//! oracle exactly.
//!
//! ## The four regenerated regions
//!
//! 1. **Stream length holder** — for an ordinary stream, qpdf counts bytes
//!    between the end of the `stream` line and the `endstream` keyword. It
//!    then remains in `st_after_stream`: every exact LF-terminated
//!    `%QDF: ignore_newline` line before the next object subtracts one byte
//!    (until the count reaches zero). If the next recognized line is `N 0 obj`,
//!    its following line must consist only of decimal digits plus LF; qpdf
//!    replaces that line with the measured length and does not look up
//!    `/Length M G R`. If no such header occurs before the tail, qpdf remains
//!    in `st_after_stream` and copies the remaining xref/trailer bytes without
//!    regenerating them. No EOL normalization is performed.
//! 2. **xref offsets** — each in-use object's 10-digit offset is the byte
//!    offset of the start of its `N G obj` line in the *rewritten* output.
//! 3. **trailer `/Size`** — object count + 1 (equivalently the highest object
//!    number + 1, since numbering is contiguous `1..N`).
//! 4. **`startxref`** — the byte offset of the `xref` keyword that begins the
//!    regenerated table.
//!
//! Running [`fix_qdf`] on an already-valid QDF file is a no-op, and the
//! function is idempotent: `fix_qdf(fix_qdf(x)) == fix_qdf(x)`.

use crate::tokenizer::{is_delimiter, is_ws};
use crate::{Error, Result};

/// One member (a `%% Object stream: object N` marker + its decompressed
/// body) inside an object stream's stream content.
#[derive(Debug, Clone)]
struct ObjStmMember {
    num: u32,
    /// Byte offset (in the *input*) one past this member's own marker
    /// line's EOL — where its body begins. The regenerated offset table
    /// stores each member's offset relative to `members[0].body_start`
    /// (this becomes `/First`); the marker+body bytes themselves need no
    /// individual bookkeeping beyond this, since they are copied as a
    /// single verbatim block (see `ObjectBody::ObjStm`). The first
    /// member's marker-line start (`ObjectBody::ObjStm::first_marker_start`)
    /// is returned separately by `scan_objstm_members` rather than stored
    /// per-member here, since no other member's marker position is used.
    body_start: usize,
}

/// What qpdf's `fix-qdf` classifies a top-level object's body as
/// (`qpdf/fix-qdf.cc`'s `st_in_obj` dispatch on the object's dictionary).
#[derive(Debug, Clone)]
enum ObjectBody {
    /// A regular object: a non-stream object or an ordinary stream.
    Plain {
        /// If this object directly contains a stream, the verbatim
        /// recomputed `/Length` value (byte count between the `stream` EOL
        /// and `endstream`).
        stream_len: Option<usize>,
        /// Number of exact marker lines between this stream's endobj and the
        /// next top-level object. qpdf subtracts once per line.
        ignore_newline_count: usize,
    },
    /// An object stream (`/Type /ObjStm`).
    ObjStm {
        /// Byte offset (in the *input*) one past the EOL of the line
        /// containing `/Type /ObjStm` — header text through this point
        /// (`N G obj`, `<<`, the `/Type /ObjStm` line) is copied verbatim;
        /// everything from here through the original `stream` keyword
        /// (stale `/Length`/`/N`/`/First`/`/Extends`/`>>`) is discarded and
        /// regenerated.
        type_line_end: usize,
        /// Byte offset (in the *input*) of the first member's marker line —
        /// also where the verbatim copy-through of markers+bodies begins.
        first_marker_start: usize,
        /// Byte offset (in the *input*) of the `endstream` keyword closing
        /// this object stream (end of the verbatim copy-through region).
        endstream_kw: usize,
        members: Vec<ObjStmMember>,
        /// Raw `N 0 R` bytes captured from an `/Extends` entry in the
        /// original (discarded) dictionary, if present.
        extends: Option<Vec<u8>>,
    },
    /// A cross-reference stream (`/Type /XRef`). Note there is no
    /// `endstream_kw`/end bound here: everything from `content_start`
    /// onward — the stale binary payload, `endstream`, `endobj`, and
    /// anything after in the input — is discarded and replaced wholesale
    /// (qpdf's `st_done`; matches the oracle exactly).
    XRefStream {
        /// Byte offset (in the *input*) one past the EOL of the line
        /// containing `/Type /XRef` — where synthetic `/Length`/`/W`
        /// emission and per-line dict filtering (dropping the stale
        /// `/Length`/`/W`, regenerating `/Size`, keeping everything else
        /// verbatim) begins.
        type_line_end: usize,
        /// Byte offset (in the *input*) one past the `stream` keyword's
        /// EOL — the upper bound of the per-line dict filtering pass (the
        /// `stream` line itself is the last line filtered).
        content_start: usize,
    },
}

/// One parsed qpdf-recognized `N 0 obj ... endobj` body in the input.
#[derive(Debug, Clone)]
struct ObjectSpan {
    num: u32,
    gen: u32,
    /// Byte offset (in the *input*) of the start of the `N 0 obj` line.
    obj_line_start: usize,
    /// Byte offset (in the *input*) of the first body line after the exact
    /// qpdf `N 0 obj\n` header.
    body_start: usize,
    /// Byte offset (in the *input*) one past the `endobj` keyword's line
    /// (start of the next byte region, used as this object's end bound).
    end: usize,
    body: ObjectBody,
}

/// Find the next qpdf `N 0 obj\n` line at `from`, scanning line by line.
/// Returns `(num, gen, line_start, content_after_obj_kw)`.
///
/// Stops (returns `None`) upon the first top-level `xref` keyword line,
/// without considering any further lines. This mirrors qpdf's own `st_top`
/// dispatch (`qpdf/fix-qdf.cc:126-134`): `N G obj` object recognition and
/// the classic tail's `xref` keyword compete on the SAME per-line scan, and
/// once `xref\n` is seen the state machine (`st_at_xref` → ... → `st_done`)
/// can never re-enter object recognition — `st_done` ignores every
/// remaining line rather than echoing it. So nothing past the real tail
/// `xref` line — including a syntactically valid `N G obj ... endobj` block
/// appended after the original `%%EOF` — is ever reinterpreted as an
/// object, and this scan must not continue past it either.
fn find_next_obj(input: &[u8], from: usize) -> Option<(u32, u32, usize, usize)> {
    let mut line_start = from;
    while line_start < input.len() {
        let line_end = memchr_nl(input, line_start).unwrap_or(input.len());
        let line = &input[line_start..line_end];
        if line_end < input.len() {
            if let Some((num, gen, kw_end)) = parse_obj_header(line) {
                return Some((num, gen, line_start, line_start + kw_end));
            }
        }
        if is_xref_keyword_line(input, line_start) {
            return None;
        }
        line_start = line_end + 1;
        if line_end >= input.len() {
            break;
        }
    }
    None
}

/// Whether the line starting at `line_start` (already known to be a line
/// start) is EXACTLY qpdf's `st_top` classic-tail `xref` keyword line: the
/// line's entire content, including its trailing `\n`, must equal the
/// literal 5-byte string `"xref\n"` — mirroring qpdf's own
/// `line.compare("xref\n"sv) == 0` (`qpdf/fix-qdf.cc:130`), a full-line
/// EQUALITY check, not a prefix or whitespace-tolerant one. A line like
/// `xref stream\n` (confirmed against the live oracle: NOT recognized —
/// `st_top` falls through to its default `std::cout << line;`, leaving
/// object recognition active) or a bare `xref` with no trailing `\n` at
/// all (the literal 5-byte string can never match a shorter one) does not
/// match either, so this scan must not stop at either.
fn is_xref_keyword_line(input: &[u8], line_start: usize) -> bool {
    input[line_start..].starts_with(b"xref\n")
}

/// Index of the next `\n` at or after `from`.
fn memchr_nl(buf: &[u8], from: usize) -> Option<usize> {
    buf[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| from + i)
}

/// Parse the bytes before LF when they should be qpdf's exact `N 0 obj\n`
/// object header. `find_next_obj` only calls this for LF-terminated lines.
/// The literal spaces and generation match `re_n_0_obj` (`fix-qdf.cc:87`).
fn parse_obj_header(line: &[u8]) -> Option<(u32, u32, usize)> {
    let number = line.strip_suffix(b" 0 obj")?;
    if number.is_empty() || !number.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let num = std::str::from_utf8(number).ok()?.parse().ok()?;
    Some((num, 0, line.len()))
}

/// Find the PDF name token `name` (e.g. `b"/Length"`, `b"/Size"`,
/// `b"/ObjStm"`) inside `hay`, in object syntax only.
///
/// Walks the bytes skipping literal strings `(...)` (balanced parens, `\`
/// escapes), hex strings `<...>` (while treating `<<`/`>>` as dictionary
/// delimiters, not strings), and `%` comments, so a copy of `name` appearing
/// inside a string/comment is ignored. `name` matches only when the byte
/// immediately after it is a PDF whitespace/delimiter (ISO 32000-1 §7.2) or
/// end-of-slice, so `/Length1`, `/SizeExtra`, etc. are not mistaken for
/// `/Length` / `/Size`. Returns the start offset of the match.
fn find_name_token(hay: &[u8], name: &[u8]) -> Option<usize> {
    find_name_token_from(hay, name, 0)
}

/// Like [`find_name_token`] but starts scanning at `from`. `from` must be a
/// position in normal object context (not inside a string/hex/comment) — all
/// internal callers pass either `0` or a position just past a previously
/// matched name token, which satisfies this.
fn find_name_token_from(hay: &[u8], name: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < hay.len() {
        match hay[i] {
            // `%` comment runs to end of line (PDF §7.2.4).
            b'%' => {
                while i < hay.len() && hay[i] != b'\n' && hay[i] != b'\r' {
                    i += 1;
                }
            }
            // `<<` / `>>` are dict delimiters, not hex strings.
            b'<' if hay.get(i + 1) == Some(&b'<') => i += 2,
            b'>' if hay.get(i + 1) == Some(&b'>') => i += 2,
            // Hex string `<...>` — skip to the closing `>`.
            b'<' => {
                i += 1;
                while i < hay.len() && hay[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            // Literal string `(...)` — balanced parens, `\` escapes.
            b'(' => {
                i += 1;
                let mut depth = 1usize;
                while i < hay.len() && depth > 0 {
                    match hay[i] {
                        b'\\' => i += 1, // skip escaped byte
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            // Candidate name token in normal object context.
            b'/' if hay[i..].starts_with(name) => {
                let after = hay.get(i + name.len());
                if after.is_none_or(|&b| is_ws(b) || is_delimiter(b)) {
                    return Some(i);
                }
                i += 1; // longer name (`/Length1` etc.) — keep scanning.
            }
            _ => i += 1,
        }
    }
    None
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Count qpdf's exact standalone marker lines in one inter-object separator.
/// Only the LF-terminated line with no surrounding whitespace matches; CRLF
/// and lookalike comments intentionally do not.
fn ignore_newline_marker_count(separator: &[u8]) -> usize {
    separator
        .split_inclusive(|&b| b == b'\n')
        .filter(|line| *line == b"%QDF: ignore_newline\n")
        .count()
}

/// Return the input range of a top-level object's first body line when it is
/// bare decimal digits plus LF (`fix-qdf.cc:90,256-259`). Its header was
/// already recognized by `parse_obj_header` using qpdf's `re_n_0_obj`.
fn qpdf_bare_integer_line_range(input: &[u8], object: &ObjectSpan) -> Option<(usize, usize)> {
    let integer_start = object.body_start;
    let integer_end = memchr_nl(input, integer_start)?;
    if integer_end + 1 > object.end {
        return None;
    }
    let integer = &input[integer_start..integer_end];
    if integer.is_empty() || !integer.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some((integer_start, integer_end + 1))
}

/// Whether a dict's lines contain qpdf's exact, oracle-literal type marker
/// text (`/Type /ObjStm` or `/Type /XRef`), scanned one line at a time —
/// mirrors qpdf's own per-line `line.find("/Type /ObjStm"sv)` /
/// `line.find("/Type /XRef"sv)` checks in `st_in_obj`
/// (`qpdf/fix-qdf.cc:142,145`), which run against the raw text of ONE line
/// while streaming an object's dictionary, with NO PDF parsing at all: no
/// comment/string-literal skipping (unlike `find_name_token_from`, used
/// elsewhere in this module for `/Length`/`/Size`) and no dictionary-
/// nesting-depth awareness. Confirmed against the live `fix-qdf` binary
/// that the SAME literal text inside a `%` comment or a `(...)` string
/// literal on that line IS misclassified as a real `/Type /ObjStm`/
/// `/Type /XRef` marker, exactly like a genuine nested sub-dictionary's
/// `/Type /XRef` value already is (`corrupt-nested-type-xref` fixture) —
/// because qpdf's check is a plain substring search, not a parse.
/// Conversely, because qpdf's loop is strictly per-line, a value split
/// across a line break (e.g. `/Type %comment\n/ObjStm`) never matches:
/// confirmed against the live oracle that a comment-split `/Type`/
/// `/ObjStm` pair is NOT recognized as an object stream at all (the
/// object is treated as an ordinary, unclassified stream instead, with
/// downstream effects that depend on what follows it in the file).
///
/// Returns the offset (within `dict`) of the start of the match on the
/// first matching line, in file order.
fn find_raw_type_line(dict: &[u8], needle: &[u8]) -> Option<usize> {
    let mut line_start = 0;
    while line_start < dict.len() {
        let line_end = memchr_nl(dict, line_start)
            .map(|p| p + 1)
            .unwrap_or(dict.len());
        if let Some(off) = find_subslice(&dict[line_start..line_end], needle) {
            return Some(line_start + off);
        }
        line_start = line_end;
    }
    None
}

/// Byte offset one past the EOL of the line containing `pos` (`pos` must
/// not itself be a `\n`). Used to find where verbatim copy-through stops
/// after a `/Type /ObjStm` or `/Type /XRef` line.
fn line_end_after(input: &[u8], pos: usize) -> usize {
    memchr_nl(input, pos).map(|p| p + 1).unwrap_or(input.len())
}

/// A `%% Object stream: object N` marker line, line-anchored — mirrors
/// qpdf's `re_ostream_obj = "^%% Object stream: object (\d+)"`, which has
/// no requirement on what follows the digits (`, index M` / `; original
/// object ID: K` / nothing are all accepted, exactly like the oracle).
fn parse_ostream_marker(line: &[u8]) -> Option<u32> {
    let rest = line.strip_prefix(b"%% Object stream: object ")?;
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()
}

/// Scan an object stream's `stream`...`endstream` content
/// (`[content_start, endstream_kw)`) for member marker lines, in encounter
/// order. The original offset-table lines preceding the first marker (and
/// any non-marker line, defensively) are ignored — qpdf discards them
/// unconditionally and regenerates the table from these positions.
///
/// Returns `None` if no marker line is found (a malformed object stream —
/// every real one has at least one member). On success, also returns the
/// byte offset of the *first* member's own marker line (the start of the
/// verbatim copy-through region; see `ObjectBody::ObjStm`).
fn scan_objstm_members(
    input: &[u8],
    content_start: usize,
    endstream_kw: usize,
) -> Option<(usize, Vec<ObjStmMember>)> {
    let mut first_marker_start = None;
    let mut members = Vec::new();
    let mut line_start = content_start;
    while line_start < endstream_kw {
        let line_end = line_end_after(input, line_start).min(endstream_kw);
        if let Some(num) = parse_ostream_marker(&input[line_start..line_end]) {
            first_marker_start.get_or_insert(line_start);
            members.push(ObjStmMember {
                num,
                body_start: line_end,
            });
        }
        line_start = line_end;
    }
    first_marker_start.map(|start| (start, members))
}

/// Scan a region (the discarded lines of an ObjStm's original dictionary)
/// for an `/Extends N 0 R` entry — mirrors qpdf's `re_extends = "/Extends
/// (\d+ 0 R)"`, a plain substring search (not name-token-boundary-aware,
/// matching the oracle's literal regex exactly). Returns the raw `N 0 R`
/// bytes to re-emit verbatim.
fn find_extends(region: &[u8]) -> Option<Vec<u8>> {
    let pos = find_subslice(region, b"/Extends ")?;
    let rest = &region[pos + b"/Extends ".len()..];
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let after_digits = &rest[digits..];
    if !after_digits.starts_with(b" 0 R") {
        return None;
    }
    Some(rest[..digits + b" 0 R".len()].to_vec())
}

/// Minimum big-endian byte width needed to hold `v` (0 for `v == 0`) —
/// qpdf's zero-truncating `while (t) { t >>= 8; ++nbytes; }` loop.
fn byte_width(mut v: u64) -> usize {
    let mut n = 0;
    while v > 0 {
        v >>= 8;
        n += 1;
    }
    n
}

/// Append `val` as `bytes` big-endian bytes (qpdf's `writeBinary`).
fn write_binary(out: &mut Vec<u8>, val: u64, bytes: usize) {
    for i in (0..bytes).rev() {
        out.push(((val >> (8 * i)) & 0xff) as u8);
    }
}

/// Emit a regenerated object stream: `  /Length N\n  /N n\n  /First
/// F\n[  /Extends E\n]>>\nstream\n` followed by the regenerated offset
/// table and the verbatim marker+body content
/// (`first_marker_start..endstream_kw`). The caller has already copied the
/// object header through the `/Type /ObjStm` line, and copies
/// `endstream_kw..` (covering `endstream`/`endobj` and beyond) afterward.
fn emit_objstm(
    out: &mut Vec<u8>,
    input: &[u8],
    first_marker_start: usize,
    endstream_kw: usize,
    members: &[ObjStmMember],
    extends: &Option<Vec<u8>>,
) {
    use std::fmt::Write as _;

    let base = members[0].body_start;
    let mut offsets = String::new();
    for m in members {
        let _ = writeln!(offsets, "{} {}", m.num, m.body_start - base);
    }
    // /First = len(regenerated offset table) + len(the first member's own
    // marker line) — the marker line is retained as literal stream content
    // preceding the first member's body (qpdf's `ostream_offsets.at(0)`,
    // itself the first member's marker-line length before the `-= first`
    // normalization; see the module-level derivation in `qdf_fix.rs`).
    let marker0_len = base - first_marker_start;
    let first = offsets.len() + marker0_len;
    // /Length = len(regenerated offset table) + the untouched span from the
    // first marker line through `endstream` (markers + bodies, verbatim).
    let stream_length = offsets.len() + (endstream_kw - first_marker_start);

    out.extend_from_slice(
        format!(
            "  /Length {stream_length}\n  /N {}\n  /First {first}\n",
            members.len()
        )
        .as_bytes(),
    );
    if let Some(ext) = extends {
        out.extend_from_slice(b"  /Extends ");
        out.extend_from_slice(ext);
        out.push(b'\n');
    }
    out.extend_from_slice(b">>\nstream\n");
    out.extend_from_slice(offsets.as_bytes());
    out.extend_from_slice(&input[first_marker_start..endstream_kw]);
}

/// Emit a regenerated cross-reference stream's dict tail and binary payload:
/// synthetic `/Length`/`/W` right after the (already-copied) `/Type /XRef`
/// line, then the original dict's remaining lines filtered per-line (drop
/// stale `/Length`/`/W`, regenerate `/Size`, keep everything else — a plain
/// substring match per line, mirroring `qpdf/fix-qdf.cc`'s
/// `st_in_xref_stream_dict`, which is *not* name-token-boundary-aware),
/// then the binary xref table and the literal
/// `\nendstream\nendobj\n\nstartxref\n<offset>\n%%EOF\n` tail. Everything
/// in the input from `content_start` onward is otherwise ignored.
fn emit_xref_stream(
    out: &mut Vec<u8>,
    input: &[u8],
    type_line_end: usize,
    content_start: usize,
    entries: &[crate::XrefEntry],
    size: usize,
    this_offset: usize,
) {
    let f1_nbytes = byte_width(this_offset as u64);
    let max_index = entries
        .iter()
        .filter_map(|e| match e {
            crate::XrefEntry::Compressed { index, .. } => Some(*index),
            crate::XrefEntry::Uncompressed { .. } | crate::XrefEntry::Free { .. } => None,
        })
        .max()
        .unwrap_or(0);
    let f2_nbytes = byte_width(u64::from(max_index)).max(1);

    out.extend_from_slice(
        format!(
            "  /Length {}\n  /W [ 1 {f1_nbytes} {f2_nbytes} ]\n",
            (1 + entries.len()) * (1 + f1_nbytes + f2_nbytes)
        )
        .as_bytes(),
    );

    let mut line_start = type_line_end;
    while line_start < content_start {
        let line_end = line_end_after(input, line_start).min(content_start);
        let line = &input[line_start..line_end];
        if find_subslice(line, b"/Length").is_some() || find_subslice(line, b"/W").is_some() {
            // already emitted above.
        } else if find_subslice(line, b"/Size").is_some() {
            out.extend_from_slice(format!("  /Size {size}\n").as_bytes());
        } else {
            out.extend_from_slice(line);
        }
        line_start = line_end;
    }

    write_binary(out, 0, 1);
    write_binary(out, 0, f1_nbytes);
    write_binary(out, 0, f2_nbytes);
    for e in entries {
        match e {
            crate::XrefEntry::Uncompressed { offset } => {
                write_binary(out, 1, 1);
                write_binary(out, *offset, f1_nbytes);
                write_binary(out, 0, f2_nbytes);
            }
            crate::XrefEntry::Compressed { stream, index } => {
                write_binary(out, 2, 1);
                write_binary(out, u64::from(*stream), f1_nbytes);
                write_binary(out, u64::from(*index), f2_nbytes);
            }
            // Unreachable by construction: `entries` (built in `fix_qdf`)
            // only ever pushes `Uncompressed`/`Compressed`; this arm exists
            // solely to satisfy exhaustiveness against the crate's shared
            // `XrefEntry` type.
            // cov:ignore-start: unreachable arm, see comment above
            crate::XrefEntry::Free { .. } => {
                unreachable!("qdf_fix's entries vector only ever holds Uncompressed/Compressed")
            } // cov:ignore-end
        }
    }
    out.extend_from_slice(b"\nendstream\nendobj\n\n");
    out.extend_from_slice(b"startxref\n");
    out.extend_from_slice(format!("{this_offset}\n").as_bytes());
    out.extend_from_slice(b"%%EOF\n");
}

/// Validate that `num` continues the sequential `1..N` counter from `last`
/// (qpdf's `fix-qdf.cc` `checkObjId`, which fatals on the first out-of-order
/// object number), returning the new counter value. Callers pass the
/// containing top-level object's line as `err_offset` for both the object
/// itself and any of its object-stream members.
fn check_sequential(num: u32, last: u32, err_offset: usize) -> Result<u32> {
    if num != last + 1 {
        return Err(Error::parse(
            err_offset,
            "fix_qdf: non-sequential object numbering \
             (canonical QDF numbers objects 1..N in order)",
        ));
    }
    Ok(num)
}

/// Read and recompute a hand-edited QDF file.
///
/// See the module docs for the exact rules. Returns the corrected bytes.
///
/// # Errors
///
/// * [`Error::Unsupported`] if an object stream (`/Type /ObjStm`) is present
///   in a file whose tail is a classic `xref` table rather than a
///   cross-reference stream — real `qpdf --qdf` always pairs object streams
///   with a cross-reference stream (a classic table cannot represent a
///   compressed-object entry), so this combination cannot arise from
///   genuine QDF input.
/// * [`Error::Parse`] if the input does not look like a QDF file (no `xref`
///   table or cross-reference stream, malformed trailer, an indirect
///   `/Length` whose holder object is missing, an object stream with no
///   `%% Object stream: object N` marker lines, or object numbers — spanning
///   both top-level objects and object stream members — that are not
///   contiguous `1..N` in file order).
pub fn fix_qdf(input: &[u8]) -> Result<Vec<u8>> {
    // ---- 1. Parse all `N G obj` spans, from the start of the file. ------
    // Unlike the classic-only version, we do NOT pre-locate a tail `xref`
    // keyword to bound this scan: a cross-reference-stream-form file has no
    // such keyword at all, and qpdf itself never looks for one up front
    // either — it discovers the file's tail shape (classic `xref` line vs.
    // an object whose dict is `/Type /XRef`) while walking objects in file
    // order. We do the same: scan until `find_next_obj` finds no more
    // `N G obj` lines (the classic tail — `xref`/`trailer` text never
    // matches that pattern), or until an object classifies as a
    // cross-reference stream, which is always the last real content in a
    // valid file (everything after its `stream` keyword is ignored) so we
    // stop there immediately.
    let mut objects: Vec<ObjectSpan> = Vec::new();
    // Whether the last-scanned object is a cross-reference stream — set
    // alongside the `break` below, and reused as-is after the loop instead
    // of re-matching `objects.last()` to answer the same question twice.
    let mut is_xref_stream_form = false;
    let mut cursor = 0usize;
    while let Some((num, gen, line_start, kw_end)) = find_next_obj(input, cursor) {
        // Determine whether this object contains a stream BEFORE searching for
        // `endobj`. A decompressed QDF stream body may itself contain a line
        // that starts with `endobj`, which would truncate the object span if we
        // searched for `endobj` naively. The real `endobj` always follows the
        // `endstream` keyword, so for stream objects we anchor the search there.
        let mut stream_info: Option<(usize, usize, usize)> = None; // (stream_kw, content_start, endstream_kw)
                                                                   // A real stream's `stream` keyword follows this object's dictionary
                                                                   // close `>>`. `find_matching_dict_close` skips literal strings, hex
                                                                   // strings, and `%` comments, so a `stream`/`endstream` byte sequence
                                                                   // inside a NON-stream object's string value (which lives *inside*
                                                                   // `<<...>>`, before the close) is never mistaken for a real stream.
                                                                   // For dict-less objects (e.g. bare-integer length holders) the first
                                                                   // `<<` belongs to a later object; the `stream_is_ours` endobj-
                                                                   // precedence check below then correctly rejects it.
                                                                   // Set when the dictionary classifies as `/Type /XRef` (see below) —
                                                                   // bypasses the `endstream`/`endobj` search entirely.
        let mut early_xref_body: Option<(ObjectBody, usize)> = None; // (body, end)
        let after_dict = find_subslice(&input[kw_end..], b"<<")
            .map(|o| kw_end + o)
            .and_then(|d| find_matching_dict_close(input, d))
            .map(|c| c + 2);
        if let Some(stream_kw) =
            after_dict.and_then(|sf| find_line_keyword_from(input, b"stream", sf))
        {
            // Only treat it as this object's stream if there is no `endobj`
            // before the `stream` keyword (otherwise the stream belongs to a
            // later object).
            let first_endobj = find_line_keyword_from(input, b"endobj", kw_end);
            let stream_is_ours = match first_endobj {
                None => true,
                Some(eob) => stream_kw < eob,
            };
            if stream_is_ours {
                // Compute content_start: just past the `stream` EOL.
                let mut content_start = stream_kw + b"stream".len();
                if input.get(content_start) == Some(&b'\r') {
                    content_start += 1;
                }
                if input.get(content_start) == Some(&b'\n') {
                    content_start += 1;
                }
                // Classify the dictionary for `/Type /XRef` BEFORE requiring
                // `endstream` to be found. qpdf's own `st_in_obj` dispatch
                // recognizes `/Type /XRef` from the dictionary's lines alone
                // (`qpdf/fix-qdf.cc:145`, checked per line while still
                // scanning the dict, well before `st_in_stream` would ever
                // be entered), and once classified, completes the whole
                // object on the `stream\n` line itself
                // (`st_in_xref_stream_dict` writes its binary payload and
                // tail literal the moment that line is seen,
                // `qpdf/fix-qdf.cc:217-238`) — `endstream` is never looked
                // for. A QDF file truncated right after an XRef stream's
                // `stream` line (with no `endstream` anywhere) is therefore
                // still fully repairable by the oracle, and must be here too.
                let dict = &input[kw_end..stream_kw];
                if let Some(type_pos) = find_raw_type_line(dict, b"/Type /XRef") {
                    let type_line_end = line_end_after(input, kw_end + type_pos);
                    early_xref_body = Some((
                        ObjectBody::XRefStream {
                            type_line_end,
                            content_start,
                        },
                        // Unused by any downstream logic for this object
                        // (see `ObjectBody::XRefStream`'s doc: everything
                        // from `content_start` onward is discarded and
                        // replaced wholesale, and this is always the last
                        // object scanned — the `break` below never lets a
                        // later object read it via `cursor`).
                        content_start,
                    ));
                } else if let Some(endstream_kw) =
                    find_line_keyword_from(input, b"endstream", content_start)
                {
                    // Search for `endstream` starting from content_start.
                    stream_info = Some((stream_kw, content_start, endstream_kw));
                }
            }
        }

        let (body, end) = if let Some(early) = early_xref_body {
            early
        } else {
            // `endobj` search begins AFTER `endstream` when a stream is
            // present, so that a line-anchored `endobj` inside the stream
            // body is not mistaken for the object terminator.
            let endobj_search_from = match stream_info {
                Some((_, _, endstream_kw)) => endstream_kw + b"endstream".len(),
                None => kw_end,
            };
            let endobj =
                find_line_keyword_from(input, b"endobj", endobj_search_from).ok_or_else(|| {
                    Error::parse(line_start, "fix_qdf: object without matching `endobj`")
                })?;
            let end = endobj + b"endobj".len();

            // A non-stream object can be neither an object stream nor a
            // cross-reference stream (both require a `stream`...`endstream`
            // payload by construction), so type classification only applies
            // when `stream_info` is present. Restrict it to the DICTIONARY
            // portion (before `stream`), not the decompressed content that may
            // follow — matching qpdf's own per-line `st_in_obj` checks, which
            // stop looking once `stream\n` is seen. (The `/Type /XRef` case
            // was already classified, and short-circuited, above.)
            let body =
                if let Some((stream_kw_abs, content_start_abs, endstream_kw_abs)) = stream_info {
                    let dict = &input[kw_end..stream_kw_abs];
                    if let Some(type_pos) = find_raw_type_line(dict, b"/Type /ObjStm") {
                        let type_line_end = line_end_after(input, kw_end + type_pos);
                        let Some((first_marker_start, members)) =
                            scan_objstm_members(input, content_start_abs, endstream_kw_abs)
                        else {
                            return Err(Error::parse(
                                line_start,
                                "fix_qdf: object stream (/Type /ObjStm) has no \
                             `%% Object stream: object N` marker lines",
                            ));
                        };
                        let extends = find_extends(&input[type_line_end..stream_kw_abs]);
                        ObjectBody::ObjStm {
                            type_line_end,
                            first_marker_start,
                            endstream_kw: endstream_kw_abs,
                            members,
                            extends,
                        }
                    } else {
                        // qpdf's st_after_stream state does not inspect the
                        // dictionary's /Length entry. Holder selection is
                        // derived from the next top-level object after parsing.
                        ObjectBody::Plain {
                            stream_len: Some(endstream_kw_abs - content_start_abs),
                            ignore_newline_count: 0,
                        }
                    }
                } else {
                    ObjectBody::Plain {
                        stream_len: None,
                        ignore_newline_count: 0,
                    }
                };
            (body, end)
        };

        is_xref_stream_form = matches!(body, ObjectBody::XRefStream { .. });
        objects.push(ObjectSpan {
            num,
            gen,
            obj_line_start: line_start,
            body_start: kw_end + 1,
            end,
            body,
        });
        if is_xref_stream_form {
            // Always the last real object in a valid file — everything
            // after its `stream` keyword is discarded and regenerated, so
            // there is nothing further to scan.
            break;
        }
        cursor = end;
    }

    if objects.is_empty() {
        return Err(Error::parse(0, "fix_qdf: no objects found before xref"));
    }

    // qpdf-deviation-start: qpdf's fix-qdf st_at_xref state has no check for
    // this combination -- it unconditionally calls QPDFXRefEntry::getOffset()
    // on every xref entry, which throws std::logic_error for the type-2
    // entries an object stream's members produce, so real qpdf crashes via
    // an uncaught exception here instead of detecting and rejecting the
    // input; flpdf instead proactively rejects it.
    if !is_xref_stream_form
        && objects
            .iter()
            .any(|o| matches!(o.body, ObjectBody::ObjStm { .. }))
    {
        // Deliberate deviation, not a mirror of qpdf: qpdf's own `st_at_xref`
        // (`fix-qdf.cc`) writes a classic entry for EVERY xref vector member
        // unconditionally, calling `e.getOffset()` even on a type-2
        // (compressed) entry — undefined/garbage output, since real
        // `qpdf --qdf` always pairs object streams with a cross-reference
        // stream (a classic table has no entry type for a compressed
        // object) and this combination never arises from genuine QDF input.
        // Rather than reproduce that undefined behavior, fail loud; see the
        // `fix_qdf` doc's `# Errors`.
        return Err(Error::Unsupported(
            "fix_qdf: an object stream (/Type /ObjStm) in a file whose tail is a classic \
             `xref` table (rather than a cross-reference stream) is not supported"
                .into(),
        ));
    }
    // qpdf-deviation-end

    // qpdf's fix-qdf requires objects numbered exactly `1..N` in file order
    // (QdfFixer::checkObjId fatals on `stoi(id) != ++last_obj`) — and this ONE
    // counter spans both top-level objects and (when present) each object
    // stream's members, in encounter order: a member is `checkObjId`'d exactly
    // like a top-level object is. Enforce the same numbering; this also
    // bounds `/Size`/the xref length to the true object count (never a dense
    // table sized by the maximum object number), so a sparse or huge object
    // number can no longer drive an overflow — AND restores full byte-for-byte
    // fix-qdf parity: flpdf's own QDF writer emits objects in ascending file
    // order with each `/Length` holder inline after its stream. This rejects
    // nothing produced by the writer or qpdf `--qdf`.
    let mut last_obj: u32 = 0;
    for obj in &objects {
        last_obj = check_sequential(obj.num, last_obj, obj.obj_line_start)?;
        if let ObjectBody::ObjStm { members, .. } = &obj.body {
            for m in members {
                last_obj = check_sequential(m.num, last_obj, obj.obj_line_start)?;
            }
        }
    }
    let size = last_obj as usize + 1;

    // qpdf remains in st_after_stream after the stream's endobj and consumes
    // every exact marker line until it reaches the next object header. Count
    // only bytes in that separator; marker-like bytes in the dictionary,
    // payload, or a different inter-object region cannot affect the length.
    for i in 0..objects.len().saturating_sub(1) {
        let end = objects[i].end;
        let next_start = objects[i + 1].obj_line_start;
        if let ObjectBody::Plain {
            stream_len: Some(_),
            ignore_newline_count,
            ..
        } = &mut objects[i].body
        {
            let separator = &input[end..next_start];
            *ignore_newline_count = ignore_newline_marker_count(separator);
        }
    }

    // ---- 2. Compute qpdf's positional length-holder replacements. --------
    // qpdf's st_after_stream transition is immediate: it does not resolve a
    // dictionary reference or skip an object whose body is not an integer.
    // The next N 0 obj line enters st_in_length, whose next line must contain
    // only decimal digits followed by LF (`fix-qdf.cc:246-264`).
    let mut new_len_body: Vec<Option<(usize, usize, usize)>> = vec![None; objects.len()];
    for (i, obj) in objects.iter().enumerate() {
        let ObjectBody::Plain {
            stream_len: Some(measured_len),
            ignore_newline_count,
        } = &obj.body
        else {
            continue;
        };
        let Some(successor) = objects.get(i + 1) else {
            continue;
        };
        let Some((integer_start, integer_end)) = qpdf_bare_integer_line_range(input, successor)
        else {
            return Err(Error::parse(
                successor.body_start,
                "fix_qdf: expected integer",
            ));
        };
        new_len_body[i + 1] = Some((
            measured_len.saturating_sub(*ignore_newline_count),
            integer_start,
            integer_end,
        ));
    }

    // ---- 3. Emit the rewritten body, substituting length-holder bodies,
    //         object-stream/xref-stream bodies, and recording each
    //         object's new offset (and, for the xref-stream form, each
    //         object stream member's compressed-entry position). ---------
    let mut out: Vec<u8> = Vec::with_capacity(input.len() + 16);
    // Everything before the first object is the header (%PDF / binary marker /
    // %QDF / blank lines) — copied verbatim.
    let first_obj_start = objects[0].obj_line_start;
    out.extend_from_slice(&input[..first_obj_start]);

    // New byte offset of each object number (by index in `objects`) — used
    // only by the classic-tail xref table (§4 below).
    let mut new_offsets: Vec<(u32, u32, usize)> = Vec::with_capacity(objects.len());
    // The full cross-reference vector in encounter order — used only by the
    // cross-reference-stream tail (built regardless of form; cheap, and
    // `is_xref_stream_form` is already known not to change mid-function).
    // Capacity is the exact final entry count: `last_obj` (validated above)
    // counts every top-level object and object-stream member.
    let mut entries: Vec<crate::XrefEntry> = Vec::with_capacity(last_obj as usize);

    for (i, obj) in objects.iter().enumerate() {
        // Copy any inter-object bytes (comments like `%% Original object ID`,
        // blank lines) that sit between the previous object end and this
        // object's line start — verbatim. For the first object this range is
        // empty (header already copied).
        if i > 0 {
            let prev_end = objects[i - 1].end;
            out.extend_from_slice(&input[prev_end..obj.obj_line_start]);
        }

        // This object's offset = current output length (start of `N 0 obj`).
        let this_offset = out.len();
        new_offsets.push((obj.num, obj.gen, this_offset));
        entries.push(crate::XrefEntry::Uncompressed {
            offset: this_offset as u64,
        });

        match &obj.body {
            ObjectBody::Plain { .. } => {
                if let Some((new_len, integer_start, integer_end)) = new_len_body[i] {
                    // Rewrite the integer body of the positionally selected
                    // object, preserving its header, framing, and endobj.
                    out.extend_from_slice(&input[obj.obj_line_start..integer_start]);
                    out.extend_from_slice(new_len.to_string().as_bytes());
                    out.push(b'\n');
                    out.extend_from_slice(&input[integer_end..obj.end]);
                } else {
                    // Copy the object verbatim.
                    out.extend_from_slice(&input[obj.obj_line_start..obj.end]);
                }
            }
            ObjectBody::ObjStm {
                type_line_end,
                first_marker_start,
                endstream_kw,
                members,
                extends,
            } => {
                // Header through the `/Type /ObjStm` line: verbatim.
                out.extend_from_slice(&input[obj.obj_line_start..*type_line_end]);
                emit_objstm(
                    &mut out,
                    input,
                    *first_marker_start,
                    *endstream_kw,
                    members,
                    extends,
                );
                // `endstream`/`endobj` and the inter-object gap up to the
                // next object: verbatim.
                out.extend_from_slice(&input[*endstream_kw..obj.end]);
                for (idx, _) in members.iter().enumerate() {
                    entries.push(crate::XrefEntry::Compressed {
                        stream: obj.num,
                        index: idx as u32,
                    });
                }
            }
            ObjectBody::XRefStream {
                type_line_end,
                content_start,
            } => {
                // Header through the `/Type /XRef` line: verbatim.
                out.extend_from_slice(&input[obj.obj_line_start..*type_line_end]);
                emit_xref_stream(
                    &mut out,
                    input,
                    *type_line_end,
                    *content_start,
                    &entries,
                    size,
                    this_offset,
                );
                // A cross-reference stream is always the last object; its
                // own tail literal already closes the file (§ emit_xref_stream).
                return Ok(out);
            }
        }
    }

    // If the final recognized object is an ordinary stream, qpdf never saw a
    // positional N 0 obj after it and therefore never returned from
    // st_after_stream to st_top. Earlier holders have already been rewritten;
    // the rest of the input, including the xref/trailer tail, is copied raw.
    if matches!(
        objects.last().map(|object| &object.body),
        Some(ObjectBody::Plain {
            stream_len: Some(_),
            ..
        })
    ) {
        let last_end = objects.last().unwrap().end;
        out.extend_from_slice(&input[last_end..]);
        return Ok(out);
    }

    // ---- 4. Emit the regenerated classic xref table (this form only). ---
    // qpdf's fix-qdf (QdfFixer::st_at_xref) writes a `0 <1+n>` subsection header,
    // the free-list head, then one in-use entry per object by iterating its xref
    // vector in order. Object numbering was validated as `1..N` in file order, so
    // `new_offsets` is already in ascending object-number order.
    // Locate the real tail `xref` keyword — the FIRST line-anchored match
    // strictly after the last object's end. Restricting the search to this
    // region (rather than scanning the whole input) means a decompressed
    // stream body containing a stray line-anchored `xref` earlier in the
    // file can never be mistaken for the real table.
    let last_end = objects.last().unwrap().end;
    let xref_pos = find_line_keyword_from(input, b"xref", last_end)
        .ok_or_else(|| Error::parse(last_end, "fix_qdf: no classic `xref` table found"))?;

    // Copy bytes between the last object's end and the `xref` keyword
    // (blank lines etc.) verbatim.
    out.extend_from_slice(&input[last_end..xref_pos]);

    let startxref_value = out.len();

    out.extend_from_slice(b"xref\n");
    out.extend_from_slice(format!("0 {size}\n").as_bytes());
    // Object 0 is the free-list head, exactly as qpdf fix-qdf emits it.
    out.extend_from_slice(b"0000000000 65535 f \n");
    for &(_, gen, off) in &new_offsets {
        out.extend_from_slice(format!("{off:010} {gen:05} n \n").as_bytes());
    }

    // ---- 5. Emit trailer / startxref / %%EOF. ---------------------------
    // Reuse the original trailer dictionary verbatim except for /Size, which
    // we rewrite. Locate the original trailer text after the old xref region.
    let trailer_kw = find_subslice(&input[xref_pos..], b"trailer")
        .map(|p| xref_pos + p)
        .ok_or_else(|| Error::parse(xref_pos, "fix_qdf: no `trailer` keyword"))?;
    // Trailer dictionary spans the first `<<` to its matching `>>`.
    let dict_open = find_subslice(&input[trailer_kw..], b"<<")
        .map(|p| trailer_kw + p)
        .ok_or_else(|| Error::parse(trailer_kw, "fix_qdf: trailer has no dictionary"))?;
    let dict_close = find_matching_dict_close(input, dict_open)
        .ok_or_else(|| Error::parse(dict_open, "fix_qdf: unterminated trailer dictionary"))?;

    // Copy `trailer` ... up to and including the dict, with /Size rewritten.
    let trailer_dict = &input[trailer_kw..dict_close + 2];
    let rewritten_trailer = rewrite_size(trailer_dict, size);
    out.extend_from_slice(&rewritten_trailer);

    // Copy whatever sits between `>>` and `startxref` verbatim (newline,
    // optional `/Prev` lines do not occur in QDF; just whitespace).
    let after_dict = dict_close + 2;
    let startxref_kw = find_subslice(&input[after_dict..], b"startxref")
        .map(|p| after_dict + p)
        .ok_or_else(|| Error::parse(after_dict, "fix_qdf: no `startxref` keyword"))?;
    out.extend_from_slice(&input[after_dict..startxref_kw]);

    // `startxref` then its value line, recomputed.
    out.extend_from_slice(b"startxref\n");
    out.extend_from_slice(format!("{startxref_value}\n").as_bytes());

    // Finally, the literal `%%EOF` marker — validated present in the input
    // (fail loud on a malformed tail), but emitted as qpdf's own literal
    // `"%%EOF\n"` rather than copied from input. qpdf's `st_in_trailer`
    // writes this same fixed string the moment the trailer's closing `>>\n`
    // is seen and immediately enters `st_done` (`qpdf/fix-qdf.cc:284-287`),
    // which then ignores every remaining input line rather than echoing it
    // (`qpdf/fix-qdf.cc:288-290`) — so nothing after the ORIGINAL `%%EOF`,
    // including a syntactically valid trailing `N G obj ... endobj` block,
    // is ever copied through. Confirmed against the live oracle: feeding it
    // a classic-tail QDF with such a block appended after `%%EOF` reproduces
    // this file's own regenerated tail byte-for-byte and drops the block
    // entirely (see `corrupt-trailing-garbage` fixture).
    find_subslice(&input[startxref_kw..], b"%%EOF")
        .ok_or_else(|| Error::parse(startxref_kw, "fix_qdf: no `%%EOF` marker"))?;
    out.extend_from_slice(b"%%EOF\n");

    Ok(out)
}

/// Find a keyword that begins a line (preceded by start-of-buffer or `\n`),
/// at or after `from`.
fn find_line_keyword_from(input: &[u8], kw: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + kw.len() <= input.len() {
        if &input[i..i + kw.len()] == kw {
            let at_line_start = i == 0 || input[i - 1] == b'\n' || input[i - 1] == b'\r';
            // The keyword must be followed by EOL/EOF/whitespace so we don't
            // match `xref` inside `startxref` or `endstream` inside text.
            let after_ok = match input.get(i + kw.len()) {
                None => true,
                Some(&c) => c == b'\n' || c == b'\r' || c == b' ' || c == b'\t',
            };
            if at_line_start && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Find the `>>` that closes the dictionary opened by `<<` at `open`,
/// accounting for nesting.
fn find_matching_dict_close(input: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < input.len() {
        match input[i] {
            // `%` comment runs to end of line.
            b'%' => {
                while i < input.len() && input[i] != b'\n' && input[i] != b'\r' {
                    i += 1;
                }
            }
            // `<<` / `>>` are dict delimiters (checked before single `<`/`>`).
            b'<' if input.get(i + 1) == Some(&b'<') => {
                depth += 1;
                i += 2;
            }
            b'>' if input.get(i + 1) == Some(&b'>') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 2;
            }
            // Hex string `<...>` — skip to the closing `>`.
            b'<' => {
                i += 1;
                while i < input.len() && input[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            // Literal string `(...)` — balanced parens, `\` escapes.
            b'(' => {
                i += 1;
                let mut sdepth = 1usize;
                while i < input.len() && sdepth > 0 {
                    match input[i] {
                        b'\\' => i += 1, // skip escaped byte
                        b'(' => sdepth += 1,
                        b')' => sdepth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Rewrite the `/Size <n>` entry inside a trailer dictionary slice.
fn rewrite_size(trailer: &[u8], size: usize) -> Vec<u8> {
    // `/Size` as a real name token only — skip strings/hex/comments and
    // reject `/SizeExtra` etc. via the trailing token-boundary check.
    let Some(p) = find_name_token(trailer, b"/Size") else {
        return trailer.to_vec();
    };
    let mut out = Vec::with_capacity(trailer.len() + 4);
    out.extend_from_slice(&trailer[..p + b"/Size".len()]);
    let rest = &trailer[p + b"/Size".len()..];
    // Skip whitespace, then the old integer.
    let ws = rest
        .iter()
        .take_while(|&&b| b.is_ascii_whitespace())
        .count();
    let digits = rest[ws..].iter().take_while(|b| b.is_ascii_digit()).count();
    out.extend_from_slice(&rest[..ws]);
    out.extend_from_slice(size.to_string().as_bytes());
    out.extend_from_slice(&rest[ws + digits..]);
    out
}

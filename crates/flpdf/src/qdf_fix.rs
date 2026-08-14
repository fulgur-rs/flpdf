//! qpdf correspondence: qpdf/fix-qdf.cc tool behavior outside libqpdf.
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
//! * objects are written as `N G obj` at the start of a line, optionally
//!   preceded by a `%% Original object ID: N G` comment line (the offset always
//!   points at the `N G obj` line, **not** the comment — verified against the
//!   `fix-qdf` oracle);
//! * object numbers are contiguous, numbered `1..N` in file order — qpdf's
//!   `fix-qdf` requires this (it aborts on the first out-of-sequence object)
//!   and [`fix_qdf`] rejects non-sequential numbering the same way;
//! * stream lengths are stored as an *indirect* reference `/Length M G R`, with
//!   the length itself living in a standalone `M G obj` whose body is a single
//!   integer (qpdf canonical QDF never inlines a direct `/Length <n>` for an
//!   actual stream — the oracle does not fix a direct length either);
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
//! 1. **Stream `/Length`** — for every stream object the length is the exact
//!    number of bytes between the end of the line containing the `stream`
//!    keyword and the `endstream` keyword: counting starts at the first byte
//!    after the `stream` keyword's end-of-line marker (`\r\n`, `\n`, or `\r`)
//!    and ends at (but excludes) the `endstream` keyword. No EOL normalization
//!    is performed. If the exact standalone line `%QDF: ignore_newline` occurs
//!    between the stream object and its immediately following length holder,
//!    one framing byte is excluded. The recomputed value is written into the
//!    indirect length object's body as a plain decimal integer (no zero
//!    padding).
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
    /// A regular object: a non-stream object, or a stream with a (possibly
    /// indirect) `/Length`.
    Plain {
        /// If this object directly contains a stream, the verbatim
        /// recomputed `/Length` value (byte count between the `stream` EOL
        /// and `endstream`).
        stream_len: Option<usize>,
        /// If this object's stream dict uses an indirect `/Length M G R`,
        /// the object number `M` that holds the length integer.
        length_holder: Option<u32>,
        /// qpdf emitted one framing LF that fix-qdf must exclude from
        /// `stream_len`.
        ignore_newline: bool,
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

/// One parsed `N G obj ... endobj` body in the input.
#[derive(Debug, Clone)]
struct ObjectSpan {
    num: u32,
    gen: u32,
    /// Byte offset (in the *input*) of the start of the `N G obj` line.
    obj_line_start: usize,
    /// Byte offset (in the *input*) one past the `endobj` keyword's line
    /// (start of the next byte region, used as this object's end bound).
    end: usize,
    body: ObjectBody,
}

/// Find the next line that begins exactly with `N G obj` at `from`, scanning
/// line by line. Returns `(num, gen, line_start, content_after_obj_kw)`.
fn find_next_obj(input: &[u8], from: usize) -> Option<(u32, u32, usize, usize)> {
    let mut line_start = from;
    while line_start < input.len() {
        let line_end = memchr_nl(input, line_start).unwrap_or(input.len());
        let line = &input[line_start..line_end];
        if let Some((num, gen, kw_end)) = parse_obj_header(line) {
            return Some((num, gen, line_start, line_start + kw_end));
        }
        line_start = line_end + 1;
        if line_end >= input.len() {
            break;
        }
    }
    None
}

/// Index of the next `\n` at or after `from`.
fn memchr_nl(buf: &[u8], from: usize) -> Option<usize> {
    buf[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| from + i)
}

/// Parse a line that should be `N G obj` (with optional trailing content after
/// the `obj` keyword, e.g. nothing in canonical QDF). Returns
/// `(num, gen, byte index just past "obj")` on success.
fn parse_obj_header(line: &[u8]) -> Option<(u32, u32, usize)> {
    // Trim a trailing '\r' (CRLF inputs).
    let line = if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    };
    let s = std::str::from_utf8(line).ok()?;
    let mut it = s.split_ascii_whitespace();
    let num: u32 = it.next()?.parse().ok()?;
    let gen: u32 = it.next()?.parse().ok()?;
    if it.next()? != "obj" {
        return None;
    }
    // canonical QDF puts nothing else on the line; reject if there is.
    if it.next().is_some() {
        return None;
    }
    let kw_end = s.rfind("obj")? + 3;
    Some((num, gen, kw_end))
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

/// Scan a stream dictionary slice for `/Length M G R` (indirect) or
/// `/Length <int>` (direct). Returns `Indirect(M)` or `Direct`.
enum LengthKind {
    /// Indirect `/Length M 0 R` — canonical QDF only ever uses generation 0.
    Indirect(u32),
    /// Indirect `/Length M G R` with `G != 0` — not canonical QDF; qdf_fix
    /// keys holders by object number only, so a non-zero generation cannot be
    /// validated/rewritten safely. Treated as an explicit error rather than
    /// silently rewriting the wrong-generation object.
    IndirectUnsupportedGeneration,
    Direct,
    None,
}

fn classify_length(dict: &[u8]) -> LengthKind {
    // Find the PDF *name token* `/Length` in object syntax (skipping strings,
    // hex strings, and comments; requiring a trailing token boundary so
    // `/Length1` etc. do not match).
    let needle = b"/Length";
    let Some(p) = find_name_token(dict, needle) else {
        return LengthKind::None;
    };
    let rest = &dict[p + needle.len()..];
    let s = match std::str::from_utf8(rest) {
        Ok(s) => s,
        Err(_) => return LengthKind::None,
    };
    let mut it = s.split_ascii_whitespace();
    let Some(first) = it.next() else {
        return LengthKind::None;
    };
    if first.parse::<u32>().is_err() {
        return LengthKind::None;
    }
    // Indirect form: `<int> <int> R`
    let second = it.next();
    let third = it.next();
    if let (Some(g), Some(r)) = (second, third) {
        if let (Ok(gen), "R") = (g.parse::<u32>(), r) {
            return if gen == 0 {
                LengthKind::Indirect(first.parse().unwrap())
            } else {
                LengthKind::IndirectUnsupportedGeneration
            };
        }
    }
    LengthKind::Direct
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Whether an inter-object separator contains qpdf's exact standalone marker.
/// qpdf's fix-qdf recognizes only the LF-terminated line with no surrounding
/// whitespace, so CRLF and lookalike comments intentionally do not match.
fn has_ignore_newline_marker(separator: &[u8]) -> bool {
    separator
        .split_inclusive(|&b| b == b'\n')
        .any(|line| line == b"%QDF: ignore_newline\n")
}

/// Find a `/Type` *name token* whose *value* is the name token `value` —
/// not merely any occurrence of `value` (it could be an unrelated name
/// value like `/SomeKey /ObjStm`, and copies in strings/comments are
/// skipped by `find_name_token_from`). Returns the position of the matched
/// *value* token (not the `/Type` key) — callers use this to find where
/// verbatim copy-through of the header should stop, and the value can sit
/// on a different line than the key (`/Type %comment\n  /ObjStm`), so the
/// line containing the value, not the key, is the one that must be kept
/// whole. Used to classify a dict as an object stream (`/ObjStm`) or a
/// cross-reference stream (`/XRef`).
fn find_type_value(body: &[u8], value: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(tp) = find_name_token_from(body, b"/Type", from) {
        let mut j = tp + b"/Type".len();
        // Skip PDF whitespace AND `%...EOL` comments between the key and its
        // value (comments are token separators too — `/Type %c\n /ObjStm`).
        loop {
            match body.get(j) {
                Some(&b) if is_ws(b) => j += 1,
                Some(&b'%') => {
                    while body.get(j).is_some_and(|&c| c != b'\n' && c != b'\r') {
                        j += 1;
                    }
                }
                _ => break,
            }
        }
        if body[j..].starts_with(value)
            && body
                .get(j + value.len())
                .is_none_or(|&b| is_ws(b) || is_delimiter(b))
        {
            return Some(j);
        }
        from = tp + b"/Type".len();
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
            crate::XrefEntry::Free { .. } => {
                unreachable!("qdf_fix's entries vector only ever holds Uncompressed/Compressed")
            }
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
                // Search for `endstream` starting from content_start.
                if let Some(endstream_kw) =
                    find_line_keyword_from(input, b"endstream", content_start)
                {
                    stream_info = Some((stream_kw, content_start, endstream_kw));
                }
            }
        }

        // `endobj` search begins AFTER `endstream` when a stream is present, so
        // that a line-anchored `endobj` inside the stream body is not mistaken
        // for the object terminator.
        let endobj_search_from = match stream_info {
            Some((_, _, endstream_kw)) => endstream_kw + b"endstream".len(),
            None => kw_end,
        };
        let endobj = find_line_keyword_from(input, b"endobj", endobj_search_from)
            .ok_or_else(|| Error::parse(line_start, "fix_qdf: object without matching `endobj`"))?;
        let end = endobj + b"endobj".len();

        // A non-stream object can be neither an object stream nor a
        // cross-reference stream (both require a `stream`...`endstream`
        // payload by construction), so type classification only applies
        // when `stream_info` is present. Restrict it to the DICTIONARY
        // portion (before `stream`), not the decompressed content that may
        // follow — matching qpdf's own per-line `st_in_obj` checks, which
        // stop looking once `stream\n` is seen.
        let body = if let Some((stream_kw_abs, content_start_abs, endstream_kw_abs)) = stream_info {
            let dict = &input[kw_end..stream_kw_abs];
            if let Some(type_pos) = find_type_value(dict, b"/ObjStm") {
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
            } else if let Some(type_pos) = find_type_value(dict, b"/XRef") {
                let type_line_end = line_end_after(input, kw_end + type_pos);
                ObjectBody::XRefStream {
                    type_line_end,
                    content_start: content_start_abs,
                }
            } else {
                let mut length_holder = None;
                match classify_length(dict) {
                    LengthKind::Indirect(m) => length_holder = Some(m),
                    LengthKind::IndirectUnsupportedGeneration => {
                        return Err(Error::parse(
                            line_start,
                            "fix_qdf: stream /Length holder with non-zero generation \
                             is not supported (canonical QDF uses generation 0)",
                        ));
                    }
                    LengthKind::Direct | LengthKind::None => {
                        // Canonical qpdf QDF always uses an indirect length for
                        // real streams; the oracle does not rewrite a direct
                        // one. Leave it untouched (verbatim preservation).
                    }
                }
                ObjectBody::Plain {
                    stream_len: Some(endstream_kw_abs - content_start_abs),
                    length_holder,
                    ignore_newline: false,
                }
            }
        } else {
            ObjectBody::Plain {
                stream_len: None,
                length_holder: None,
                ignore_newline: false,
            }
        };

        is_xref_stream_form = matches!(body, ObjectBody::XRefStream { .. });
        objects.push(ObjectSpan {
            num,
            gen,
            obj_line_start: line_start,
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

    // qpdf's fix-qdf requires objects numbered exactly `1..N` in file order
    // (QdfFixer::checkObjId fatals on `stoi(id) != ++last_obj`) — and this ONE
    // counter spans both top-level objects and (when present) each object
    // stream's members, in encounter order: a member is `checkObjId`'d exactly
    // like a top-level object is. Enforce the same numbering; this also
    // bounds `/Size`/the xref length to the true object count (never a dense
    // table sized by the maximum object number), so a sparse or huge object
    // number can no longer drive an overflow — AND restores full byte-for-byte
    // fix-qdf parity: flpdf's own QDF writer emits objects in ascending file
    // order with each `/Length` holder inline after its stream
    // (flpdf-abu3 / PR #430), so this rejects nothing the writer (or qpdf
    // `--qdf`) produces.
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

    // qpdf writes the marker after the stream object's `endobj` and directly
    // before its synthetic length holder. Associate only that exact separator
    // with the stream; marker-like bytes in the dictionary, payload, or a
    // different inter-object region cannot affect its length.
    for i in 0..objects.len().saturating_sub(1) {
        let next_num = objects[i + 1].num;
        let end = objects[i].end;
        let next_start = objects[i + 1].obj_line_start;
        if let ObjectBody::Plain {
            length_holder: Some(h),
            ignore_newline,
            ..
        } = &mut objects[i].body
        {
            if *h == next_num {
                let separator = &input[end..next_start];
                *ignore_newline = has_ignore_newline_marker(separator);
            }
        }
    }

    // ---- 2. Compute the new length-holder integer bodies. ---------------
    // Validate every indirect `/Length M G R` holder (flpdf-9hc.25):
    //   * the holder object `M` must actually exist in the parsed set —
    //     otherwise the "repaired" file still carries a dangling indirect
    //     length and is invalid for downstream readers; and
    //   * a holder reused by two streams with *conflicting* lengths is an
    //     explicit error rather than silent last-writer-wins (which would
    //     leave the earlier stream's /Length wrong).
    // A canonical QDF indirect /Length is always `M 0 R` (generation 0;
    // non-zero generations are rejected above). The holder must therefore be
    // an object whose number is M AND whose generation is 0 — matching on the
    // number alone would wrongly accept/rewrite an `M G` object with G != 0.
    // Only TOP-LEVEL objects are eligible holders — provably so, not just by
    // convention: in `fix-qdf.cc`, `st_in_length` (where an indirect
    // /Length's holder value is written) is reachable only from
    // `st_after_stream` matching `re_n_0_obj`, and `st_after_stream` is only
    // reachable from `st_top`/`st_in_obj` (top-level object states). The
    // `st_in_ostream_*` states used while inside an object stream have no
    // transition into `st_after_stream`/`st_in_length` at all, so qpdf's own
    // state machine can never treat a compressed member as a length holder.
    // A holder number that resolves to one therefore correctly fails loud as
    // "missing" below.
    let gen0_object_numbers: std::collections::HashSet<u32> = objects
        .iter()
        .filter(|o| o.gen == 0)
        .map(|o| o.num)
        .collect();
    let mut new_len_body: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for obj in &objects {
        if let ObjectBody::Plain {
            stream_len: Some(measured_len),
            length_holder: Some(holder),
            ignore_newline,
        } = &obj.body
        {
            let len = if *ignore_newline {
                measured_len.saturating_sub(1)
            } else {
                *measured_len
            };
            if !gen0_object_numbers.contains(holder) {
                return Err(Error::parse(
                    obj.obj_line_start,
                    "fix_qdf: stream's indirect /Length holder object (M 0) is missing",
                ));
            }
            if let Some(&prev) = new_len_body.get(holder) {
                if prev != len {
                    return Err(Error::parse(
                        obj.obj_line_start,
                        "fix_qdf: indirect /Length holder reused with conflicting lengths",
                    ));
                }
            }
            new_len_body.insert(*holder, len);
        }
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

        // This object's offset = current output length (start of `N G obj`).
        let this_offset = out.len();
        new_offsets.push((obj.num, obj.gen, this_offset));
        entries.push(crate::XrefEntry::Uncompressed {
            offset: this_offset as u64,
        });

        match &obj.body {
            ObjectBody::Plain { .. } => {
                // Only a generation-0 object can be the holder a canonical
                // `M 0 R` /Length points at — never rewrite an `M G` object
                // with G != 0.
                if let Some(&new_len) = new_len_body.get(&obj.num).filter(|_| obj.gen == 0) {
                    // Rewrite this length-holder object: keep the `N G obj`
                    // line and `endobj`, replace the integer body with the
                    // recomputed value.
                    rewrite_length_holder(&mut out, &input[obj.obj_line_start..obj.end], new_len)?;
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

    // Finally the `%%EOF` (and any trailing bytes) copied verbatim.
    let eof = find_subslice(&input[startxref_kw..], b"%%EOF")
        .map(|p| startxref_kw + p)
        .ok_or_else(|| Error::parse(startxref_kw, "fix_qdf: no `%%EOF` marker"))?;
    out.extend_from_slice(&input[eof..]);

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

/// Given the verbatim bytes of a length-holder object (`N G obj\n<int>\nendobj`
/// possibly with different whitespace), emit it with the integer replaced by
/// `new_len`, preserving the `obj`/`endobj` lines and surrounding whitespace.
fn rewrite_length_holder(out: &mut Vec<u8>, obj_bytes: &[u8], new_len: usize) -> Result<()> {
    // Find end of the `N G obj` header line.
    let nl = obj_bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| Error::parse(0, "fix_qdf: malformed length object header"))?;
    // Header (including its newline) copied verbatim.
    out.extend_from_slice(&obj_bytes[..=nl]);

    // The body is everything up to the `endobj` keyword. Preserve leading and
    // trailing whitespace around the integer so the file shape is kept.
    let endobj_rel = find_line_keyword_from(obj_bytes, b"endobj", nl + 1)
        .ok_or_else(|| Error::parse(0, "fix_qdf: length object missing endobj"))?;
    let body = &obj_bytes[nl + 1..endobj_rel];

    // Split body into leading whitespace, the integer token, trailing bytes.
    let lead = body
        .iter()
        .take_while(|&&b| b.is_ascii_whitespace())
        .count();
    let after_int = body[lead..]
        .iter()
        .position(|&b| !b.is_ascii_digit())
        .map(|p| lead + p)
        .unwrap_or(body.len());
    // Sanity: the token between lead..after_int must be all digits.
    if after_int == lead || !body[lead..after_int].iter().all(|b| b.is_ascii_digit()) {
        return Err(Error::parse(
            0,
            "fix_qdf: length-holder body is not a plain integer",
        ));
    }
    out.extend_from_slice(&body[..lead]);
    out.extend_from_slice(new_len.to_string().as_bytes());
    out.extend_from_slice(&body[after_int..]);

    // Emit `endobj` and the rest of the object verbatim.
    out.extend_from_slice(&obj_bytes[endobj_rel..]);
    Ok(())
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

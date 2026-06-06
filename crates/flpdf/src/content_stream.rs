//! Content-stream tokenizer (ISO 32000-1 §7.8.2).
//!
//! A PDF content stream is a sequence of operands followed by an operator,
//! interleaved with inline images and comments. This module turns the raw
//! bytes into a stream of [`ContentToken`]s without normalising or
//! re-serialising anything — it is the shared foundation for the downstream
//! normalize / coalesce / resource-scan passes.
//!
//! The operand lexer (numbers, strings, names, arrays, dictionaries,
//! booleans, `null`) is reused verbatim from [`crate::parser`]: content
//! streams use exactly the same object syntax minus indirect references
//! (`N G R` never appears in a content stream).
//!
//! # Example
//!
//! ```
//! use flpdf::content_stream::{ContentStreamParser, ContentToken};
//! use flpdf::Object;
//!
//! let mut tokens = ContentStreamParser::new(b"1 0 0 1 72 720 cm\nBT /F1 12 Tf ET");
//! match tokens.next().unwrap().unwrap() {
//!     ContentToken::Op { operands, operator } => {
//!         assert_eq!(operator, b"cm");
//!         assert_eq!(operands.len(), 6);
//!         assert_eq!(operands[0], Object::Integer(1));
//!     }
//!     other => panic!("expected cm op, got {other:?}"),
//! }
//! ```

use crate::parser::{is_delimiter, is_ws, Parser};
use crate::{Dictionary, Error, Object, Result};

/// One lexical unit of a content stream.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentToken {
    /// An operator and the operands that preceded it.
    ///
    /// `operator` is the raw keyword bytes (e.g. `b"cm"`, `b"Tj"`, `b"'"`).
    Op {
        operands: Vec<Object>,
        operator: Vec<u8>,
    },
    /// An inline image: `BI` … `ID` … `EI`.
    ///
    /// `dict` holds the inline-image parameters with their **abbreviated**
    /// names preserved (`/W`, `/H`, `/BPC`, `/CS`, …) — they are *not*
    /// expanded to their long forms. `data` is the raw, untouched payload
    /// between the single whitespace byte after `ID` and the `EI` keyword.
    /// The one whitespace byte after `ID` **and** the single whitespace byte
    /// immediately before `EI` (the separators) are excluded from `data`, so
    /// a round-tripping consumer must re-insert a separator on each side.
    InlineImage { dict: Dictionary, data: Vec<u8> },
    /// A `%` comment up to (not including) the end-of-line.
    ///
    /// Only emitted when [`ContentParseOptions::keep_comments`] is `true`.
    /// The leading `%` is not included in the bytes.
    Comment(Vec<u8>),
}

/// Tokenizer behaviour knobs.
///
/// `Default` yields `keep_comments == false` (comments stripped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContentParseOptions {
    /// When `true`, `%` comments are emitted as [`ContentToken::Comment`].
    /// When `false` (the default) they are skipped silently.
    pub keep_comments: bool,

    /// When `true`, a malformed token does **not** fuse the iterator: the
    /// parser skips one byte past the offending position and resumes, so later
    /// well-formed operators are still produced ("skip malformed, last-wins").
    /// Operands accumulated before the bad token are preserved.
    ///
    /// When `false` (the default) the iterator fuses on the first error, which
    /// is the safe choice for general content-stream consumers where a bad
    /// token leaves the operand stack in an unreliable state. Recovery is
    /// intended for tolerant scanners such as the `/DA` parser, mirroring
    /// qpdf's `allow_bad` tokenizer behaviour.
    ///
    /// Forward progress is guaranteed: each recovered error advances the cursor
    /// by at least one byte, so the iterator always terminates.
    pub recover_from_errors: bool,
}

/// Streaming content-stream tokenizer.
///
/// Implements [`Iterator`] yielding `Result<ContentToken>`. Iteration stops
/// (returns `None`) at end of input; a malformed token yields `Some(Err(_))`
/// and, unless [`ContentParseOptions::recover_from_errors`] is set, the
/// iterator then terminates. With recovery enabled the error is still yielded
/// but the iterator skips past the offending byte and continues.
pub struct ContentStreamParser<'a> {
    input: &'a [u8],
    pos: usize,
    options: ContentParseOptions,
    /// Operands accumulated since the last operator/image/comment.
    operands: Vec<Object>,
    /// Set once an error has been produced so the iterator fuses.
    done: bool,
}

impl<'a> ContentStreamParser<'a> {
    /// Create a tokenizer over `input` with default options
    /// (comments stripped).
    pub fn new(input: &'a [u8]) -> Self {
        Self::with_options(input, ContentParseOptions::default())
    }

    /// Create a tokenizer over `input` with explicit options.
    pub fn with_options(input: &'a [u8], opts: ContentParseOptions) -> Self {
        Self {
            input,
            pos: 0,
            options: opts,
            operands: Vec::new(),
            done: false,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.input.get(self.pos + offset).copied()
    }

    /// Skip whitespace. If `keep_comments` is false, also skip `%` comments.
    /// Returns `Some(comment_bytes)` when a comment was encountered and
    /// `keep_comments` is true (caller should emit it).
    fn skip_ws_collect_comment(&mut self) -> Option<Vec<u8>> {
        loop {
            while matches!(self.peek(), Some(byte) if is_ws(byte)) {
                self.pos += 1;
            }
            if self.peek() == Some(b'%') {
                let start = self.pos + 1;
                while !matches!(self.peek(), None | Some(b'\n' | b'\r')) {
                    self.pos += 1;
                }
                if self.options.keep_comments {
                    return Some(self.input[start..self.pos].to_vec());
                }
                // Skipped: keep looping (there may be more ws/comments).
                continue;
            }
            return None;
        }
    }

    /// Read the next bare keyword (operator) token: bytes up to the next
    /// whitespace or delimiter. Caller has already verified the current byte
    /// does not start an operand.
    fn read_keyword(&mut self) -> Vec<u8> {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if is_ws(byte) || is_delimiter(byte) {
                break;
            }
            self.pos += 1;
        }
        self.input[start..self.pos].to_vec()
    }

    /// Does the byte at the current position begin a PDF operand
    /// (number, string, name, array, dict, or `true`/`false`/`null`)?
    fn at_operand_start(&self) -> bool {
        match self.peek() {
            None => false,
            Some(byte) => match byte {
                b'/' | b'(' | b'[' => true,
                b'<' => true, // hex string or `<<` dictionary
                b'+' | b'-' | b'.' | b'0'..=b'9' => true,
                b't' => self.keyword_operand(b"true"),
                b'f' => self.keyword_operand(b"false"),
                b'n' => self.keyword_operand(b"null"),
                _ => false,
            },
        }
    }

    /// Is the input at `pos` exactly the keyword `kw` followed by a token
    /// boundary (EOF, whitespace, or a delimiter)? Without the boundary
    /// check an extension operator like `nullop` or `trueColor` would be
    /// mis-split into a `null`/`true` operand plus a shorter operator.
    fn keyword_operand(&self, kw: &[u8]) -> bool {
        let rest = &self.input[self.pos..];
        rest.starts_with(kw)
            && match rest.get(kw.len()) {
                None => true,
                Some(b) => is_ws(*b) || is_delimiter(*b),
            }
    }

    /// Parse a single operand using the shared object lexer.
    fn parse_operand(&mut self) -> Result<Object> {
        let mut parser = Parser::new_no_reference(&self.input[self.pos..]);
        let obj = parser.parse_one_object()?;
        self.pos += parser.position();
        Ok(obj)
    }

    /// Parse the inline-image dictionary (bare `/Key value` pairs) up to the
    /// Consume whitespace and `%` comments unconditionally, never emitting
    /// a comment. Used in contexts (inline-image header) where a comment is
    /// not a standalone token: with `keep_comments == true`,
    /// [`Self::skip_ws_collect_comment`] returns at the comment's line end
    /// without consuming the newline, so a single call would leave the
    /// parser stuck on whitespace. Looping drains every comment/whitespace
    /// run regardless of the option.
    fn skip_ws_and_comments(&mut self) {
        while self.skip_ws_collect_comment().is_some() {}
    }

    /// `ID` keyword, then collect raw data up to the `EI` keyword.
    fn parse_inline_image(&mut self) -> Result<ContentToken> {
        let mut dict = Dictionary::new();
        loop {
            self.skip_ws_and_comments();
            match self.peek() {
                None => {
                    return Err(Error::parse(self.pos, "inline image missing ID"));
                }
                Some(b'/') => {
                    // Key
                    let mut parser = Parser::new_no_reference(&self.input[self.pos..]);
                    let key = match parser.parse_one_object()? {
                        Object::Name(name) => name,
                        _ => return Err(Error::parse(self.pos, "inline image key is not a name")),
                    };
                    self.pos += parser.position();
                    // Value
                    self.skip_ws_and_comments();
                    let value = self.parse_operand()?;
                    dict.insert(key, value);
                }
                Some(_) => {
                    // Expect the `ID` keyword.
                    let kw = self.read_keyword();
                    if kw == b"ID" {
                        break;
                    }
                    return Err(Error::parse(
                        self.pos,
                        format!(
                            "unexpected token {:?} in inline image header",
                            String::from_utf8_lossy(&kw)
                        ),
                    ));
                }
            }
        }

        // Skip exactly one whitespace byte after `ID`. Per ISO 32000-1
        // §7.8.2 the data begins after a single whitespace character; we
        // treat a `\r\n` pair as one separator (matching qpdf), otherwise a
        // bare `\n` data byte would be lost.
        match self.peek() {
            Some(b'\r') if self.peek_at(1) == Some(b'\n') => self.pos += 2,
            Some(byte) if is_ws(byte) => self.pos += 1,
            _ => {}
        }

        let data_start = self.pos;
        // Scan for `EI` bounded by whitespace/delimiter/EOF on both sides.
        // A naive search for "EI" is wrong: image bytes can contain "EI".
        loop {
            match self.peek() {
                None => {
                    return Err(Error::parse(self.pos, "inline image missing EI"));
                }
                Some(b'E') if self.peek_at(1) == Some(b'I') => {
                    // Per ISO 32000-1 §7.8.2 the terminating `EI` must be
                    // preceded by whitespace. Accepting a delimiter here
                    // would false-match binary image data containing byte
                    // sequences like `>EI ` and truncate the image.
                    let prev_ok = self.pos == data_start
                        || self.input.get(self.pos - 1).is_some_and(|b| is_ws(*b));
                    let after = self.peek_at(2);
                    let after_ok = after.is_none_or(|b| is_ws(b) || is_delimiter(b));
                    if prev_ok && after_ok {
                        // Data excludes the whitespace separator that
                        // precedes `EI`. This must mirror the post-`ID`
                        // separator handling: a `\r\n` pair is one
                        // separator (strip 2 bytes), otherwise a single
                        // whitespace byte (strip 1). Stripping only one
                        // byte off a `\r\n` terminator would leave a
                        // stray `\r` in `data` and change the payload on
                        // re-serialization.
                        let mut data_end = self.pos;
                        if data_end >= data_start + 2
                            && self.input.get(data_end - 2) == Some(&b'\r')
                            && self.input.get(data_end - 1) == Some(&b'\n')
                        {
                            data_end -= 2;
                        } else if data_end > data_start
                            && self.input.get(data_end - 1).is_some_and(|b| is_ws(*b))
                        {
                            data_end -= 1;
                        }
                        let data = self.input[data_start..data_end].to_vec();
                        self.pos += 2; // consume `EI`
                        return Ok(ContentToken::InlineImage { dict, data });
                    }
                    self.pos += 1;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Produce the next token, or `None` at end of input.
    fn next_token(&mut self) -> Option<Result<ContentToken>> {
        loop {
            if let Some(comment) = self.skip_ws_collect_comment() {
                return Some(Ok(ContentToken::Comment(comment)));
            }
            if self.peek().is_none() {
                if self.operands.is_empty() {
                    return None;
                }
                // Trailing operands with no operator: malformed stream.
                return Some(Err(Error::parse(
                    self.pos,
                    "content stream ended with dangling operands",
                )));
            }

            if self.at_operand_start() {
                match self.parse_operand() {
                    Ok(obj) => {
                        self.operands.push(obj);
                        continue;
                    }
                    Err(err) => return Some(Err(err)),
                }
            }

            // Otherwise it is an operator keyword.
            let keyword = self.read_keyword();
            if keyword.is_empty() {
                // A lone delimiter that did not start an operand (e.g. a
                // stray `}`); cannot make progress.
                return Some(Err(Error::parse(
                    self.pos,
                    "unexpected delimiter in content stream",
                )));
            }

            if keyword == b"BI" {
                // An inline image takes no operands. Operands sitting in
                // the buffer before `BI` mean the stream is malformed;
                // surface that rather than silently discarding them.
                if !self.operands.is_empty() {
                    return Some(Err(Error::parse(
                        self.pos,
                        "inline image operator BI cannot have operands",
                    )));
                }
                return Some(self.parse_inline_image());
            }

            let operands = std::mem::take(&mut self.operands);
            return Some(Ok(ContentToken::Op {
                operands,
                operator: keyword,
            }));
        }
    }
}

impl<'a> Iterator for ContentStreamParser<'a> {
    type Item = Result<ContentToken>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let item = self.next_token();
        match item {
            Some(Err(_)) if self.options.recover_from_errors && self.pos < self.input.len() => {
                // Best-effort recovery: skip one byte past the offending
                // position so the next call makes progress, and do NOT fuse.
                // Operands accumulated before the bad token are kept so a
                // following operator still sees them. `pos` strictly increases
                // each error, guaranteeing termination.
                self.pos += 1;
            }
            Some(Err(_)) | None => {
                self.done = true;
            }
            Some(Ok(_)) => {}
        }
        item
    }
}

/// Normalize a PDF content stream into a canonical, one-operator-per-line form.
///
/// # Normalization rules
///
/// 1. Comments are stripped (equivalent to `keep_comments = false`).
/// 2. Each operator is emitted on its own line, preceded by its operands
///    separated by single ASCII spaces.  The line is terminated with `\n`.
/// 3. Operands are serialized with [`Object::write_pdf`]: integers as decimal,
///    reals via `f64::to_string()` (see note below), names as `/Name`, literal
///    strings as `(…)`, binary strings as `<hex>`, arrays and dictionaries in
///    the standard PDF syntax.
/// 4. Inline images are re-emitted as `BI\n /K v\n …\n ID\n<raw-data>\nEI\n`.
///    The raw image bytes are written verbatim (no encoding); one `\n` separator
///    is inserted after `ID` and before `EI`, as required by ISO 32000-1 §7.8.2.
///
/// # Observable-equivalence vs. byte-equality with qpdf
///
/// The goal is **observable equivalence** (re-parsing the output yields the same
/// operator sequence and operand values as the input), *not* byte-for-byte
/// identity with qpdf's `--normalize-content` output. Known divergences:
///
/// - **Integer-valued reals**: `f64::to_string()` drops trailing `.0`, so
///   `Real(1.0)` is serialized as `"1"` and re-parsed as `Integer(1)`. This is
///   semantically identical for all PDF operators. qpdf preserves the decimal
///   point for integer-valued reals; flpdf does not.
/// - **Dictionary key ordering**: `Dictionary` uses `BTreeMap` (lexicographic
///   order). qpdf may preserve insertion order in some cases.
/// - **Token separation**: a single space is always emitted between operands,
///   regardless of whether adjacent tokens are PDF delimiters. qpdf may omit
///   spaces between adjacent delimiter tokens (e.g. `>>`/`<<`). Both forms
///   parse identically.
/// - **Inline image dict key ordering**: same BTreeMap-lex caveat as above.
///
/// The output is idempotent: `normalize(normalize(x)) == normalize(x)` for all
/// well-formed inputs (byte-identical after the first pass).
///
/// # Errors
///
/// Returns an error if `input` is not a well-formed content stream
/// (propagated from [`ContentStreamParser`]).
pub fn normalize_content_stream(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    for token in ContentStreamParser::new(input) {
        match token? {
            ContentToken::Op { operands, operator } => {
                // Emit all operands space-separated, then the operator, then newline.
                for (i, operand) in operands.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    operand.write_pdf(&mut out);
                }
                if !operands.is_empty() {
                    out.push(b' ');
                }
                out.extend_from_slice(&operator);
                out.push(b'\n');
            }
            ContentToken::InlineImage { dict, data } => {
                // BI header
                out.extend_from_slice(b"BI\n");
                for (key, value) in dict.iter() {
                    out.extend_from_slice(b" /");
                    out.extend_from_slice(key);
                    out.push(b' ');
                    value.write_pdf(&mut out);
                    out.push(b'\n');
                }
                // ID separator (one \n counts as the required whitespace byte
                // per ISO 32000-1 §7.8.2; the parser strips exactly one ws byte
                // after ID and one before EI, so we must re-insert one on each side)
                out.extend_from_slice(b"ID\n");
                out.extend_from_slice(&data);
                // EI separator then EI keyword
                out.push(b'\n');
                out.extend_from_slice(b"EI\n");
            }
            ContentToken::Comment(_) => {
                // Comments are always stripped by normalize; the parser is
                // constructed with keep_comments=false (the default), so this
                // branch is unreachable in practice.
            }
        }
    }
    Ok(out)
}

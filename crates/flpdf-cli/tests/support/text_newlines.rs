//! Collapse host text-transport CRLF pairs for qpdf differential comparisons.
//!
//! Prefer the `EOL` constant from `support/eol.rs` instead whenever the
//! expected side of an assertion is a literal or `format!`-built string:
//! writing `{EOL}` into the
//! expectation *pins* the platform's observable bytes, which is strictly
//! stronger than erasing them. This helper exists only for the comparisons
//! that have no literal expectation to pin — both operands are runtime output,
//! and the two sides can reach the test through *different* host transports:
//!
//! * `qpdf.exe`'s own C-runtime stdout/stderr are opened in text mode, so on
//!   Windows every `\n` it writes becomes `\r\n`. flpdf's shared CLI logger
//!   reproduces that conversion (`crates/flpdf/src/logger.rs`, whose text-mode
//!   flag is initialized to `cfg!(windows)`), so raw byte comparison of two
//!   *CLI* processes is in fact safe on both platforms; the helper documents
//!   that platform boundary at the assertion site rather than changing it.
//! * qpdf's CLI writes `--json` files through that same text-mode path, while
//!   `qpdf-ctest` test46/47 deliberately use a binary `FILE*` and Rust writes
//!   bare LF. Those two genuinely disagree on Windows.
//! * An in-process recording pipeline receives the logger's logical LF bytes
//!   even where the CLI transport would have translated them.
//!
//! In every case the assertion is about the text payload, not about the host
//! transport convention. Only CRLF *pairs* are collapsed; a lone `\r` is
//! preserved, so genuine carriage returns inside the payload still compare.
//! Never route PDF output bytes through this helper: qpdf's `\n`/`\r\n` choices
//! inside a PDF are observable output under test and must be compared raw.

pub fn normalize_text_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;

    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'\r' && rest.first() == Some(&b'\n') {
            normalized.push(b'\n');
            remaining = &rest[1..];
        } else {
            normalized.push(byte);
            remaining = rest;
        }
    }

    normalized
}

//! Production tokenizer types and helpers exposed only to qtest helper binaries.
//! qpdf correspondence: QPDFTokenizer.hh, test_tokenizer.cc

pub use crate::tokenizer::{Token, TokenType, Tokenizer, TokenizerStateError};

pub fn token_type_name(token_type: TokenType) -> &'static str {
    match token_type {
        TokenType::Bad => "bad",
        TokenType::ArrayClose => "array_close",
        TokenType::ArrayOpen => "array_open",
        TokenType::BraceClose => "brace_close",
        TokenType::BraceOpen => "brace_open",
        TokenType::DictClose => "dict_close",
        TokenType::DictOpen => "dict_open",
        TokenType::Integer => "integer",
        TokenType::Name => "name",
        TokenType::Real => "real",
        TokenType::String => "string",
        TokenType::Null => "null",
        TokenType::Bool => "bool",
        TokenType::Word => "word",
        TokenType::Eof => "eof",
        TokenType::Space => "space",
        TokenType::Comment => "comment",
        TokenType::InlineImage => "inline-image",
    }
}

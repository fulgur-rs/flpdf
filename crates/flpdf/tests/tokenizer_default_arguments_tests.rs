use flpdf::tokenizer::{TokenType, Tokenizer};

#[test]
fn read_token_uses_qpdf_default_options() {
    let mut default_options = Tokenizer::new(b"token ");
    let token = default_options
        .read_token()
        .expect("read token with qpdf defaults");

    let mut explicit_options = Tokenizer::new(b"token ");
    let explicit = explicit_options
        .read_token_with_options(false, 0)
        .expect("read token with explicit qpdf defaults");

    assert_eq!(token.token_type, TokenType::Word);
    assert_eq!(token.value, b"token");
    assert_eq!(token.raw, b"token");
    assert_eq!(token.token_type, explicit.token_type);
    assert_eq!(token.value, explicit.value);
    assert_eq!(token.raw, explicit.raw);

    let mut long_input = vec![b'x'; 5_000];
    long_input.push(b' ');
    let long_token = Tokenizer::new(&long_input)
        .read_token()
        .expect("max_len=0 places no token length limit");
    assert_eq!(long_token.value.len(), 5_000);

    assert!(Tokenizer::new(b"(").read_token().is_err());
}

#[test]
fn read_token_with_options_preserves_bad_token_and_max_length_behavior() {
    let mut tokenizer = Tokenizer::new(b"abcdef ");
    let token = tokenizer
        .read_token_with_options(true, 3)
        .expect("allow bad token at the configured limit");

    assert_eq!(token.token_type, TokenType::Bad);
    assert_eq!(
        token.error_message.as_deref(),
        Some(&b"exceeded allowable length while reading token"[..])
    );

    let mut strict_tokenizer = Tokenizer::new(b"abcdef ");
    assert!(strict_tokenizer.read_token_with_options(false, 3).is_err());
}

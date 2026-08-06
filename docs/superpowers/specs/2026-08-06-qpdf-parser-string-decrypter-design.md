# QPDFParser StringDecrypter design

## Goal

Port qpdf 11.9.0's file-object-parser string decryption boundary to
`flpdf-25kg.3.17`. The canonical `ObjectHandle` parser must decrypt every
`tt_string` exactly once as it is tokenized, retain signature `/Contents`
ciphertext and its parsed offset, and let the canonical resolver supply the
object-specific cipher.

This design follows qpdf's responsibility split rather than extending
flpdf's existing post-parse recursive string walker.

## qpdf model

`QPDF::readObject` constructs `QPDF::StringDecrypter` only for an encrypted
document and binds it to the object generation. It supplies that object to
`QPDFParser`, which calls `decryptString` for `tt_string` values at both the
top level and inside containers. It never calls the decrypter for a
`tt_word`, including object-mode recovery that represents an unknown word as
a string.

While parsing an encrypted dictionary, qpdf records the raw token bytes and
token start for `/Contents`. On dictionary completion it restores those raw
bytes only when the final dictionary is a signature dictionary: `/Type /Sig`,
`/ByteRange`, and a string-valued `/Contents` are all present. Thus every
other string stays decrypted.

Sources: `include/qpdf/QPDFObjectHandle.hh:192-200`,
`libqpdf/qpdf/QPDFParser.hh:14-90`, `libqpdf/QPDFParser.cc:96-121,260-265,
327-365`, `libqpdf/QPDF.cc:165-175,1331-1340`, and
`libqpdf/QPDF_encryption.cc:977-1039`.

## Architecture

### Parser contract

Add a crate-private, fallible `StringDecrypter` contract to `parser.rs` and
thread an optional mutable instance through the live file-object parser.
The parser owns the invocation point: it invokes the contract only from the
`TokenType::String` arms, before constructing the corresponding
`ObjectHandle`. The optional value is absent for explicit direct parsing,
object-stream parsing, and content-stream parsing.

The contract operates on token bytes in place and returns `Result<()>`.
Its failure returns through the parser boundary without conversion to a
sentinel, panic, or post-parse repair.

### Signature sideband

Extend the live dictionary frame with an optional raw `/Contents` token and
its parsed offset. The parser captures it only when a decrypter is present
and the dictionary key is `/Contents`; it decrypts the token normally in the
same step. `finish_dictionary` evaluates the completed dictionary and
restores the raw value plus offset only for the qpdf signature predicate.

The state lives in the parser frame, matching qpdf's `StackFrame`; no raw
Object walk or parallel provenance structure is introduced.

### Resolver adapter

The canonical resolver creates an adapter for the indirect object currently
being read. The adapter uses the shared `ResolverCore` encryption cell and
the existing `EncryptionState::string_method` and `with_object_cipher`
operations to preserve qpdf's method selection, per-object key choice, and
error propagation. It is passed to the live parser only when the document is
encrypted.

This is the sole production consumer of the new parser contract. The legacy
post-parse walker is not extended or used by the canonical parse route; its
full consumer removal remains the resolver migration work in
`flpdf-25kg.3.5`.

## Rejected approach

Extending `decrypt_object_value_strings` is rejected. It runs after parsing,
cannot preserve signature ciphertext at the token boundary, decrypts
recovered unknown words incorrectly, and does not establish the qpdf
one-token/one-call contract.

## Tests

Start with qpdf differential fixtures and focused parser/resolver tests that
prove:

- RC4, AES-128, and AES-256 decrypt top-level, array, nested dictionary, and
  stream-dictionary strings exactly once;
- signature `/Contents` retains original ciphertext and the token's parsed
  offset while peer strings are plaintext;
- malformed signature dictionaries do not restore `/Contents`;
- an unknown word and content-stream token do not invoke the decrypter;
- decrypter failures propagate as parser errors; and
- no stream-payload decryption is introduced.

Run the focused tests during RED→GREEN, then formatter, relevant crate tests,
workspace tests, qpdf differential coverage, and changed-line coverage.

## Scope boundaries

Out of scope: stream payload decryption, new cryptographic primitives,
ObjectStream/cache completion, and general legacy consumer removal. The
design keeps the approved dependency graph unchanged:
`flpdf-25kg.3.17 -> flpdf-25kg.3.18` and
`flpdf-25kg.3.5 -> flpdf-25kg.3.17`.

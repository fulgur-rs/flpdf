# AES Encrypted String Wire Serialization Design

## Context

`flpdf-a32l` owns qpdf 11.9.0's writer-side encrypted-string emission
responsibility. qpdf keeps the current object data key on `QPDFWriter`,
encrypts each string while recursively unparsing the object, and chooses the
wire representation after encryption:

- AES ciphertext uses `QPDF_String::unparse(true)` and is always hexadecimal.
- RC4 ciphertext uses `QPDF_String::unparse()` and retains the normal content
  heuristic.
- object-stream members do not receive an individual data key.
- the `/Encrypt` dictionary is not encrypted, but its `/O`, `/U`, `/OE`, `/UE`,
  and `/Perms` binary values are independently serialized as hexadecimal.

The pinned source is authoritative:

- `libqpdf/QPDF_String.cc:72-105`
- `libqpdf/QPDFWriter.cc:785-803,842-847,1567-1599,1761-1796,2244-2255`

The existing branch commit `620adb03` adds a `force_hex_strings` boolean after
mutating a cloned `Object` tree. That closes the common compact-output symptom
but does not consume `WriterEncryptionState`, does not cover QDF serialization,
and deliberately disables forced hexadecimal output for the `/Encrypt`
dictionary. PR #650's Windows convergence failure exposed the last gap.

## Chosen Design

### 1. Callback-based scalar string serialization

Keep the public, infallible `Object::write_pdf` and existing plain QDF methods
unchanged. Add crate-private fallible variants that preserve every existing
container, number, name, reference, stream-dictionary, and QDF formatting rule
while delegating only `Object::String` emission to a caller callback.

The callback receives the plaintext bytes and writes the complete PDF string
token to the destination. This gives `QPDFWriter::unparseObject`-equivalent
placement without adding an encrypted-string `Object` variant or mutating the
object tree before serialization. Compact objects, QDF objects, compact stream
dictionaries, and QDF stream dictionaries use the same callback contract.

### 2. Writer-owned encrypted-string emitter

Add `writer/encrypted_strings.rs`. `EncryptedStringEmitter` owns the merged
`WriterEncryptionState` primitive from `flpdf-3yn9.11` plus the cipher kind and
IV policy from `EncryptionContext`.

For a top-level emitted object it performs this lifecycle:

1. derive and install the data key from the emitted object number and
   generation zero;
2. recursively serialize the object;
3. for every string, encrypt a temporary byte buffer with the current key;
4. serialize AES ciphertext with `write_hex_string`, or RC4 ciphertext with
   `write_string_value`;
5. clear the current data key on success or error.

An object-stream member runs the serializer with no individual key and keeps
the normal string representation. The ObjStm container's payload encryption is
unchanged and remains the only encryption applied to its members.

`EncryptionContext` retains the actual `/V` and `/R` values selected by each
builder. This lets it construct `WriterEncryptionState` without representative
or sentinel values. The currently supported copy-encryption source is V=4,
R=4 AES-128 and records those exact values.

Stream payload encryption and the cleartext metadata exemption remain on their
existing path. Moving those stages behind current-data-key state belongs to
`flpdf-3yn9.12`, not this issue.

### 3. Dedicated `/Encrypt` dictionary emission

Add a writer-owned helper corresponding to
`QPDFWriter::writeEncryptionDictionary`. It emits the dictionary in compact
sorted form, applies no object data key, and forces hexadecimal syntax only for
direct string values under `/O`, `/U`, `/OE`, `/UE`, and `/Perms`.

Other values retain their normal syntax. This matters for
`--copy-encryption-from`, whose donor dictionary is currently copied wholesale;
forcing every string in that dictionary would broaden qpdf's five-key rule.

The helper is shared by full rewrite and linearized output. qpdf also emits the
encryption dictionary in compact form under `--qdf`, so no QDF-specific
dictionary layout is added here.

## Data Flow

```text
EncryptionContext
  -> EncryptedStringEmitter(WriterEncryptionState, cipher, IV policy)
  -> with_object_data_key(emitted number, ObjStm member index)
  -> Object/Dictionary callback serializer
  -> encrypt temporary scalar bytes
  -> AES: <hex> | RC4: normal heuristic | no key: plaintext heuristic

/Encrypt Dictionary
  -> dedicated plaintext dictionary writer
  -> /O /U /OE /UE /Perms: <hex>
  -> all other values: normal serializer
```

## Error Handling

- Cipher errors propagate as `crate::Result`; they are not converted to panic.
- AES key-length conversion uses the existing `Unsupported` error contract.
- `WriterEncryptionState::with_object_data_key` clears state after callback
  failure.
- No sentinel data key, encrypted-string tag, or fallback to plaintext is
  introduced.
- The linearization convergence iteration limit is not increased. Stable wire
  lengths come from deterministic representation selection.

## Test Design

Tests must fail against `620adb03` before production changes:

1. callback serializer tests prove nested compact and QDF strings are handled
   by the supplied writer and propagate errors;
2. encrypted scalar tests use controlled printable ciphertext bytes to prove
   AES is forced to hex while RC4 retains the normal heuristic;
3. emitter tests prove current-data-key lifecycle, generation-zero derivation,
   source object immutability, and ObjStm-member exclusion;
4. `/Encrypt` tests use printable `/O` and a printable unrelated custom string
   to prove exact five-key forcing rather than dictionary-wide forcing;
5. full-rewrite compact/QDF and linearized regression tests exercise the real
   production routes, including PR #650's convergence scenario;
6. qpdf 11.9.0 probes cover AES-128, RC4, AES-256, and QDF output;
7. focused tests, workspace tests, formatting, denied-warning clippy, module-doc
   correspondence checks, and fresh 100% changed executable-line coverage are
   required before handoff.

## Non-goals

- ObjectHandle consumer cutover (`flpdf-egzr.3.2.15`)
- stream-encryption state cutover and metadata exemption (`flpdf-3yn9.12`)
- PlAesPdf/PlRc4 production cutover (`flpdf-qynx.10`)
- expanding copy-encryption beyond its existing V=4 AES-128 scope
- changing linearization layout or convergence iteration count

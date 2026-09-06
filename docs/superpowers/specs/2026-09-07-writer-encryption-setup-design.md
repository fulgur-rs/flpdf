# Shared qpdf Writer Encryption Setup Design

Status: approved for implementation on 2026-09-07.

## Goal

Translate qpdf 11.9.0's one-time `QPDFWriter::doWriteSetup` and shared
encryption parameter state into the Rust writer lifecycle. Explicit
encryption, copied/preserved encryption, and mode normalization must be
decided once before standard or linearized dispatch; route-specific
`/Encrypt` object-number placement remains a physical output concern.

## Oracle contract

- `QPDFWriter::setEncryptionParameters` and
  `copyEncryptionParameters` generate or recover the file identifier before
  converging through `setEncryptionParametersInternal`
  (`libqpdf/QPDFWriter.cc:591-702,777-840`). The internal map, file key,
  V/R, AES mode, metadata policy, and minimum version are one writer state.
- `QPDFWriter::doWriteSetup` runs once
  (`libqpdf/QPDFWriter.cc:2059-2184`): linearized clears QDF; PCLm disables
  encryption/preservation and stream transforms; QDF/normalization/decode
  normalizes stream settings; explicit encryption wins over preservation;
  forced versions disable incompatible encryption; special streams and ObjStm
  policy are prepared; page/root ObjStm exclusions and final version follow.
- `QPDFWriter::write` invokes setup once, then `prepareFileForWrite`, then
  chooses standard or linearized output (`libqpdf/QPDFWriter.cc:2187-2200`).
- Standard and linearized routes allocate the `/Encrypt` output slot at
  different physical points (`QPDFWriter.cc:2610,2794,3017`); sharing the
  parameter state must not force one incorrect object number onto both routes.

## Design

Add a writer-owned `WriterSetupState` built after `prepared_write_options` and
before `initializeSpecialStreams`/`prepareFileForWrite`. It contains the
normalized writer options' immutable encryption result and the one generated
ID seed needed by both consumers. The existing builders are split into
`EncryptionParameters` (dictionary, key, cipher, V/R, ID0, metadata policy)
and `EncryptionContext::with_encrypt_ref`, so parameter construction and
route-specific slot allocation are separate responsibilities. No new
encryption algorithm or dictionary builder is introduced.

`PdfWriter::write` passes the setup state by value into either the standard
full-rewrite coordinator or the linearized writer. The standard route creates
its context after body/renumber planning at the same output slot it currently
uses. The linearized route consumes the same parameters and assigns the slot
through its existing local renumber reservation before pass emission. Neither
route rebuilds passwords, donor state, file keys, or random ID material.

PCLm and incompatible forced-version/QDF/normalization modes are resolved in
the common prepared options before encryption setup. `prepareFileForWrite`
and `initializeSpecialStreams` remain common lifecycle steps; this slice does
not merge xref, body, or linearization layout owners.

## Invariants and verification

- Explicit encryption takes precedence over preservation; copy/preserve uses
  authenticated donor ID0/file key and the existing canonical copy builder.
- PCLm, QDF, normalization, and non-none decode policy disable preservation
  exactly as qpdf; forced versions disable incompatible handlers without
  changing the shared builder.
- A single setup consumes each required random draw once. The same ID0 feeds
  key derivation and every emitted trailer; deterministic-ID + encryption keeps
  the existing qpdf error boundary.
- Standard and linearized `/Encrypt` object numbers remain route-specific, but
  their dictionaries, file keys, cipher state, metadata exemption, and
  generation order derive from one setup state.
- RED/GREEN tests cover explicit/copy/preserve precedence, mode disables,
  forced-version behavior, deterministic-ID errors, standard and linearized
  Encrypt slots, and qpdf byte/structural parity. Existing qtest exceptions
  `.48.45` are out of scope.


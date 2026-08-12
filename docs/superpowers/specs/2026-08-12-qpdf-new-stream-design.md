# qpdf `newStream` Owned Factory Design

**Issue:** `flpdf-25kg.3.21`

## Goal

Add the `Pdf`-owned equivalent of qpdf 11.9.0 `QPDF::newStream()` without
using the legacy cloning allocator or an empty-buffer sentinel. The factory
must create one resolved stream object allocation, register that allocation
under a fresh generation-zero object identity, and retain qpdf's original
source/no-data state until explicit stream-data replacement.

## qpdf authority

qpdf's public contract documents three `newStream` forms and the mutable empty
dictionary boundary (`include/qpdf/QPDF.hh:319-340`). The implementation first
constructs an empty `QPDF_Stream` with `parsed_offset == 0` and `length == 0`,
then registers the same object allocation through
`makeIndirectFromQPDFObject` (`libqpdf/QPDF.cc:1912-1931`). The stream
constructor stores the dictionary, source offset, and length without creating
a data buffer (`libqpdf/QPDF_Stream.cc:109-137`).

The no-data state is operational: `QPDF_Stream::pipeStreamData` rejects a
stream with no replacement buffer, provider, or nonzero parsed source offset
with `pipeStreamData called for stream with no data`
(`libqpdf/QPDF_Stream.cc:571-607`). A later buffer replacement installs the
buffer and applies qpdf's `/Length` boundary through
`replaceFilterData` (`libqpdf/QPDF_Stream.cc:640-684`).

## Chosen flpdf path

1. `Pdf::new_stream()` asks the document resolver to build one direct
   resolver-associated `ObjectValue::Stream` containing an empty dictionary,
   `stream_data: None`, and `stream_length: 0`.
2. It records parsed offset `0` before promotion. `0` is the qpdf no-source
   state; `NO_PARSED_OFFSET` and `Some(Rc::new(Vec::new()))` are not equivalent.
3. It calls the existing canonical
   `Pdf::make_indirect_from_object_handle`, which delegates to
   `ResolverHandle::make_indirect_from_object_handle`. That primitive allocates
   the fresh object number, promotes the existing slot in place, and inserts
   the same handle allocation into the canonical resolver cache.
4. `Pdf::new_stream_with_data(Rc<Vec<u8>>)` delegates to `new_stream()` and
   then calls `replace_stream_data(data, None, None)`. The existing replacement
   boundary retains the exact `Rc` and removes `/Length` for zero bytes or
   writes the exact positive length.

The public `Pdf::make_indirect_object_handle` remains unchanged. Its value
cloning behavior is a temporary legacy consumer route owned by
`flpdf-25kg.3.6`; this feature must not add another compatibility bridge or
make that route the implementation mechanism.

## Scope boundaries

- Add only the owned empty factory and the `Rc<Vec<u8>>` buffer convenience.
- Do not add `StreamDataProvider`, provider storage, filterable/pipeline
  factories, retry/count/length validation, Filespec migration, or a direct
  stream API variant.
- Prove the shared handle identity at the canonical boundary, the exact
  parsed-offset/no-data behavior, allocation exhaustion, repeated distinct
  identities, owner drop, dictionary mutation, buffer sharing, and writer
  visibility after reachability from the root/trailer graph.

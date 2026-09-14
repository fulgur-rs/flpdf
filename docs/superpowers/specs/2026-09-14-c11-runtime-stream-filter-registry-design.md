# C11 runtime stream-filter registry design

## Goal

Port qpdf 11.9.0's public `QPDF::registerStreamFilter` contract into flpdf,
then make the existing ObjectHandle stream planner consume one registry for
built-in and user-registered decode filters. The fixed match must not remain as
a second production registry.

## qpdf oracle

The pinned source is `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, commit
`3b97c9bd266b7c32ea36d3536e22dab77412886d`; `/usr/bin/qpdf` is version 11.9.0.

- `QPDF::registerStreamFilter` is a public static function taking a canonical
  name and a factory (`include/qpdf/QPDF.hh:186-194`). Its implementation
  delegates to `QPDF_Stream::registerStreamFilter`
  (`libqpdf/QPDF.cc:295-300`).
- `QPDF_Stream::filter_factories` is a process-global `std::map` initialized
  with seven built-ins and updated by key assignment
  (`libqpdf/QPDF_Stream.cc:72-94,147-152`). Existing names are replaceable and
  registration is decode-only.
- `filterable` expands aliases, looks up and constructs every factory before
  reading `/DecodeParms`, returns false after an unknown factory, then applies
  each full handle parameter in order (`libqpdf/QPDF_Stream.cc:419-485`).
- `QPDFStreamFilter` has a null-only default `setDecodeParms`, a pure virtual
  decode-pipeline factory, and false defaults for specialized/lossy compression
  (`include/qpdf/QPDFStreamFilter.hh:26-66`,
  `libqpdf/QPDFStreamFilter.cc:3-19`). The filter owns the returned pipeline.
- qpdf source contains no mutex, lock-poisoning, or thread-local registry
  semantics. A Rust synchronization mechanism is a safety implementation
  detail and must not be documented as qpdf behavior.

## Rust API and ownership decision

Expose the qpdf-shaped extension boundary as a public `StreamFilter` trait,
public `PipelineRef`/`OwnedDecodePipeline` ownership handles, and a public
`register_stream_filter` function. The trait keeps qpdf's four methods:

```rust
pub trait StreamFilter: 'static {
    fn set_decode_params(&mut self, decode_params: &ObjectHandle) -> Result<bool>;
    fn get_decode_pipeline<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>>;
    fn is_specialized_compression(&self) -> bool { false }
    fn is_lossy_compression(&self) -> bool { false }
}
```

`register_stream_filter` accepts qpdf's canonical internal name bytes,
including the leading slash, and a `Send + Sync + 'static` factory returning a
`Result<S>`. The PDF object model stores decoded names without the slash, so
lookup adds it only internally; alias expansion happens before lookup.

The registry uses one safe process-global map. Lookup clones a factory and
releases the mutex before invoking user code. Built-in filters use private
adapters so the public trait does not expose flpdf's warning-callback hook;
custom filters only see the qpdf-shaped methods. No unregister operation,
partial result, retry budget, output/memory limit, or encoder registry is
added.

## Non-goals

- Do not change canonical ObjectHandle source/provider, decryption, writer, or
  raw passthrough behavior.
- Do not preserve the old fixed match as an independent route.
- Do not claim qpdf thread-safety semantics that are absent from the source.

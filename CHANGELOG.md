# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-05-13

### Added

- Initial release: pure-Rust PDF toolkit modeled on qpdf, providing a reader
  (`Pdf::open`, `pages::page_refs`, `fonts::font_entries`,
  `filters::decode_stream_data`), an incremental writer (`write_pdf`,
  `write_qdf`), and a diagnostics pass (`check_reader`).
- `flpdf-cli` binary with `pages`, `dump-object`, `qdf`, `rewrite`,
  `show-info`, `show-catalog`, `show-metadata`, `show-stream` subcommands,
  mirroring the qpdf-equivalent inspection and rewrite surface.
- Encrypted-PDF support via Standard handler V1/V2/V4/V5 (RC4 / AES) behind
  the `--password` family of CLI flags.
- Linearization writer with hint stream generation.
- Optional `qpdf-zlib-compat` feature for byte-identical FlateDecode output
  against qpdf's `compress2()`.

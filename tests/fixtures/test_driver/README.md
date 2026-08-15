# flpdf-test-driver fixtures

Small, deterministic PDFs for the Rust port of qpdf 11.9.0's
`test_driver test_0_1`. `crates/flpdf-qtest-tools/tests/driver_goldens.rs`
compares the Rust binary with the committed `.out` files during ordinary
`cargo test`; `scripts/qpdf-test-driver-diff.sh` regenerates or checks those
outputs against the pinned qpdf source.

`fixture-names.txt` is the single inventory consumed by the generator, Rust
golden test, and pinned differential script. `generate.sh --check` regenerates
every PDF in a temporary directory, compares it byte-for-byte with the
committed authored input, and requires both the `.pdf` and `.out` inventories
to match the manifest exactly. Only `qpdf-test-driver-diff.sh --regenerate`
may create or update `.out` files.

`stream_flate_error` deliberately declares `/FlateDecode` for the literal
payload `abc`. It captures qpdf's recoverable codec-error behavior: the stream
is filterable, the decoder warning is emitted at the stream-data offset, and
the filtered pipeline is still finished.

`stream_filter_error_then_warning` decodes the literal payload `78G` through
`[/ASCIIHexDecode /FlateDecode]`. ASCIIHex emits one byte before rejecting
`G`; cleanup then finishes Flate. The golden fixes qpdf's diagnostic order:
the write error precedes Flate's downstream finish warning.

`stream_flate_png_finish_warning_order` truncates a Flate trailer around a
partial PNG predictor row. It fixes qpdf's finish order: the Flate warning is
emitted before the predictor pads and writes `A\0`.

`stream_crypt_identity` and
`stream_crypt_identity_decode_parms_array` cover qpdf's driver view after
stream decryption: a valid explicit `/Crypt` filter is an identity stage.
Ordinary flpdf decode continues rejecting `/Crypt`.

`stream_flate_nondict_decode_parms` and
`stream_lzw_nondict_decode_parms_array` pin qpdf's two type warnings at the
non-dictionary parameter token before decoded output.

`missing_pdf_header`, `leading_material_pdf_header`, `missing_startxref`, and
`dict_indirect_value_warning` pin repair-warning lifecycle and the lazy
dictionary-child diagnostic boundary. The leading-material fixture keeps xref
offsets relative to the valid header and pins qpdf's first-1024-byte search plus
logical input origin.

`dict_duplicate_key` repeats a dictionary key and pins qpdf's
`dictionary has duplicated key` warning: the offset is the parser's dictionary
frame offset (right after `<<`), not the repeated key token's own offset, and
the last occurrence's value wins.

`stream_asciihex_odd_nibble_recovery` decodes `4G ` through `/AHx`. ASCIIHex
reports the invalid `G` during `write`, then its cleanup flushes the pending
odd nibble as `@`; the golden fixes the warning-before-cleanup-byte order.

`stream_asciihex_data_before_error` decodes `3431G` through `[/AHx /AHx]`.
The downstream ASCIIHex decoder emits `A` before the upstream decoder reports
its `G` write error; the golden fixes that data-before-warning relationship.

`stream_filter_chain_17` declares 17 supported `ASCIIHexDecode` stages. It
pins qpdf test 1's uncapped `qpdf_dl_all` filter construction while the ordinary
flpdf decode API remains capped at 16 by default.

`stream_deep_invalid_filter` has 64 direct nested Filter arrays. qpdf treats
the first nested array as an invalid immediate filter item; it warns and
continues instead of traversing the nested structure.

`stream_decode_parms_direct_null` and `stream_decode_parms_indirect_null`
place a null `/Predictor` directly and behind one indirect reference. qpdf's
dictionary `getKeys()` omits both, so the Flate decoder uses its defaults.

`stream_unsupported_filter_skips_decode_parms` pairs a valid-looking Flate
stage with `/BogusDecode` and uses an indirect DecodeParms dictionary whose
Flate-only `/Predictor` value is dangling. qpdf rejects the chain before
looking up that dictionary or its consumed key.

## Regeneration

```sh
bash tests/fixtures/test_driver/generate.sh
bash scripts/qpdf-test-driver-diff.sh --regenerate
```

`generate.sh` uses Python 3's standard library to calculate xref offsets and
zlib payloads. It never invokes flpdf to generate the input bytes. Check
provenance without rewriting committed files with:

```sh
bash tests/fixtures/test_driver/generate.sh --check
```

## Licensing

Every PDF in this directory is authored by flpdf and inherits the
repository's Apache-2.0/MIT license. No PDF or expected-output file is copied
from qpdf-qtest's Artistic-2.0 corpus; qpdf is used only as an executable
behavioral oracle.

## Scope

The fixtures cover missing/direct/indirect null, booleans, every scalar
Object kind used by `test_0_1`, array/dictionary child indirectness, raw and
decoded streams, indirect filter/decode-parameter shapes, malformed
top-level bare references, shallow-invalid Filter shapes, unfilterable streams,
and pre-dispatch xref repair.
The separate `test_3` fixture belongs to Bead `flpdf-n9t0.6` and is not
generated here.

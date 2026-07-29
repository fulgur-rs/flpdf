# flpdf-test-driver fixtures

Small, deterministic PDFs for the Rust port of qpdf 11.9.0's
`test_driver test_0_1`. `crates/flpdf-qtest-tools/tests/driver_goldens.rs`
compares the Rust binary with the committed `.out` files during ordinary
`cargo test`; `scripts/qpdf-test-driver-diff.sh` regenerates or checks those
outputs against the pinned qpdf source.

`stream_flate_error` deliberately declares `/FlateDecode` for the literal
payload `abc`. It captures qpdf's recoverable codec-error behavior: the stream
is filterable, the decoder warning is emitted at the stream-data offset, and
the filtered pipeline is still finished.

## Regeneration

```sh
bash tests/fixtures/test_driver/generate.sh
bash scripts/qpdf-test-driver-diff.sh --regenerate
```

`generate.sh` uses Python 3's standard library to calculate xref offsets and
zlib payloads. It never invokes flpdf to generate the input bytes.

## Licensing

Every PDF in this directory is authored by flpdf and inherits the
repository's Apache-2.0/MIT license. No PDF or expected-output file is copied
from qpdf-qtest's Artistic-2.0 corpus; qpdf is used only as an executable
behavioral oracle.

## Scope

The fixtures cover missing/direct/indirect null, booleans, every scalar
Object kind used by `test_0_1`, array/dictionary child indirectness, raw and
decoded streams, indirect filter/decode-parameter shapes, malformed
top-level bare references, unfilterable streams, and pre-dispatch xref repair.
The separate `test_3` fixture belongs to Bead `flpdf-n9t0.6` and is not
generated here.

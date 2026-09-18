# PCLm writer fixtures

`mini-pclm-in.pdf` and `mini-pclm-direct-root-in.pdf` are two-page documents
with two image strips per page. Each page's `/Resources /XObject` dictionary is
written with **descending** keys (`/Sb` before `/Sa`, `/Sd` before `/Sc`), so a
writer that seeds strips in source insertion order numbers its output
differently from qpdf, which walks `getKeys()` in ascending order
(`QPDFWriter::enqueueObjectsPCLm`, `libqpdf/QPDFWriter.cc:2927-2955`). The two
inputs differ only in the trailer `/Root`: indirect in the first, a direct
Catalog dictionary in the second.

`mini-pclm-nondict-kid-in.pdf` makes the first `/Pages /Kids` entry the direct
integer `42`. qpdf's `getAllPagesInternal` treats any kid without `/Kids` as a
page leaf, promotes a direct kid to an indirect page object, and seeds it as
the first PCLm page (`libqpdf/QPDF_pages.cc:91-131`), so the golden starts
`%PDF-1.3\n%PCLm 1.0\n1 0 obj\n42\nendobj`.

The goldens are qpdf 11.9.0 output with `setStaticID(true)`:

| golden | writer configuration |
| --- | --- |
| `mini-pclm-out.pdf` | `setPCLm(true)` |
| `mini-pclm-qdf.pdf` | `setPCLm(true)` + `setQDFMode(true)` |
| `mini-pclm-objstm.pdf` | `setPCLm(true)` + `setObjectStreamMode(qpdf_o_generate)` |
| `mini-pclm-direct-root-out.pdf` | `setPCLm(true)`, direct-Catalog input |
| `mini-pclm-ext-indirect-objstm.pdf` | `setPCLm(true)` + `setObjectStreamMode(qpdf_o_generate)`, indirect Catalog `/Extensions` |
| `mini-pclm-nondict-kid-out.pdf` | `setPCLm(true)`, direct integer `/Kids` leaf |

qpdf's CLI has no `--pclm` flag, so `generate.sh` builds a small C++ oracle
against the pinned qpdf headers and the system `libqpdf.so.29`, and self-checks
it by reproducing qpdf's own `qtest/qpdf/pclm-out.pdf` byte for byte before
writing any golden here.

PCLm output is never DEFLATE-compressed — `QPDFWriter::doWriteSetup` clears
`compress_streams` for PCLm (`libqpdf/QPDFWriter.cc:2071-2076`) — so these
goldens are independent of the zlib backend and need no `qpdf-zlib-compat`
feature gate.

Run `./generate.sh` to rebuild the inputs and goldens.

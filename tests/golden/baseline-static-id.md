# Static-ID Baseline (flpdf vs qpdf --static-id)

`flpdf-sha` is a stable 64-bit FNV-1a fingerprint of flpdf's output.
Changes to flpdf output (even those that keep the verdict label and the
length / first-diff summary the same) flip this column and fail the
baseline test, so silent drift is caught.

| fixture | flpdf-sha | flpdf bytes | golden bytes | verdict | first-diff |
|---|---|---|---|---|---|
| one-page.pdf | 8bbf34c257fe1141 | 1670 | 1189 | diverge | length mismatch (flpdf=1670 golden=1189) |
| two-page.pdf | b0f21618b84b835f | 2149 | 1579 | diverge | length mismatch (flpdf=2149 golden=1579) |
| three-page.pdf | 5e6e94efa6d229b8 | 2628 | 1970 | diverge | length mismatch (flpdf=2628 golden=1970) |
| attachment-two-page.pdf | 8e9f438c4417c29e | 2581 | 2153 | diverge | length mismatch (flpdf=2581 golden=2153) |
| linearized-one-page.pdf | - | - | - | skip | no golden for --static-id |
| encrypted-r4-three-page.pdf | - | - | - | skip | no golden for --static-id |

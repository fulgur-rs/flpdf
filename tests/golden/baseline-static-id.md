# Static-ID Baseline (flpdf vs qpdf --static-id)

`flpdf-sha` is a stable 64-bit FNV-1a fingerprint of flpdf's output.
Changes to flpdf output (even those that keep the verdict label and the
length / first-diff summary the same) flip this column and fail the
baseline test, so silent drift is caught.

| fixture | flpdf-sha | flpdf bytes | golden bytes | verdict | first-diff |
|---|---|---|---|---|---|
| one-page.pdf | 6bc0805f3469b603 | 1193 | 1189 | diverge | length mismatch (flpdf=1193 golden=1189) |
| two-page.pdf | 485e8da9e6006098 | 1587 | 1579 | diverge | length mismatch (flpdf=1587 golden=1579) |
| three-page.pdf | b467cfd3de2c6f3c | 1982 | 1970 | diverge | length mismatch (flpdf=1982 golden=1970) |
| attachment-two-page.pdf | 9f8662f8d5de15b3 | 2155 | 2153 | diverge | length mismatch (flpdf=2155 golden=2153) |
| linearized-one-page.pdf | - | - | - | skip | no golden for --static-id |
| encrypted-r4-three-page.pdf | - | - | - | skip | no golden for --static-id |

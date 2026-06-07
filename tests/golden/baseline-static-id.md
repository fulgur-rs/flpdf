# Static-ID Baseline (flpdf vs qpdf --static-id)

`flpdf-sha` is a stable 64-bit FNV-1a fingerprint of flpdf's output.
Changes to flpdf output (even those that keep the verdict label and the
length / first-diff summary the same) flip this column and fail the
baseline test, so silent drift is caught.

| fixture | flpdf-sha | flpdf bytes | golden bytes | verdict | first-diff |
|---|---|---|---|---|---|
| one-page.pdf | 30521f3ad971e35e | 1193 | 1189 | diverge | length mismatch (flpdf=1193 golden=1189) |
| two-page.pdf | 4ba5ce08d49843db | 1587 | 1579 | diverge | length mismatch (flpdf=1587 golden=1579) |
| three-page.pdf | da37d409ffc09c0d | 1982 | 1970 | diverge | length mismatch (flpdf=1982 golden=1970) |
| attachment-two-page.pdf | b49495f9e50e6294 | 2161 | 2153 | diverge | length mismatch (flpdf=2161 golden=2153) |
| linearized-one-page.pdf | - | - | - | skip | no golden for --static-id |
| encrypted-r4-three-page.pdf | - | - | - | skip | no golden for --static-id |

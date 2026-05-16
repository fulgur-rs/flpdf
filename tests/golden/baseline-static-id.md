# Static-ID Baseline (flpdf vs qpdf --static-id)

`flpdf-sha` is a stable 64-bit FNV-1a fingerprint of flpdf's output.
Changes to flpdf output (even those that keep the verdict label and the
length / first-diff summary the same) flip this column and fail the
baseline test, so silent drift is caught.

| fixture | flpdf-sha | flpdf bytes | golden bytes | verdict | first-diff |
|---|---|---|---|---|---|
| one-page.pdf | fdb01346b94f8cc2 | 1217 | 1189 | diverge | length mismatch (flpdf=1217 golden=1189) |
| two-page.pdf | 18fa4638a6243919 | 1611 | 1579 | diverge | length mismatch (flpdf=1611 golden=1579) |
| three-page.pdf | 0b5000ccab151069 | 2006 | 1970 | diverge | length mismatch (flpdf=2006 golden=1970) |
| attachment-two-page.pdf | 18017064c480ca19 | 2185 | 2153 | diverge | length mismatch (flpdf=2185 golden=2153) |
| linearized-one-page.pdf | - | - | - | skip | no golden for --static-id |
| encrypted-r4-three-page.pdf | - | - | - | skip | no golden for --static-id |

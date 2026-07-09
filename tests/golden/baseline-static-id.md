# Static-ID Baseline (flpdf vs qpdf --static-id)

`flpdf-sha` is a stable 64-bit FNV-1a fingerprint of flpdf's output.
Changes to flpdf output (even those that keep the verdict label and the
length / first-diff summary the same) flip this column and fail the
baseline test, so silent drift is caught.

| fixture | flpdf-sha | flpdf bytes | golden bytes | verdict | first-diff |
|---|---|---|---|---|---|
| one-page.pdf | 861b0bbfeee8368a | 1192 | 1189 | diverge | length mismatch (flpdf=1192 golden=1189) |
| two-page.pdf | 52022d9ba6e99359 | 1585 | 1579 | diverge | length mismatch (flpdf=1585 golden=1579) |
| three-page.pdf | 198eee6c8c5c12ff | 1979 | 1970 | diverge | length mismatch (flpdf=1979 golden=1970) |
| attachment-two-page.pdf | 55219fb715b7733c | 2152 | 2153 | diverge | length mismatch (flpdf=2152 golden=2153) |
| linearized-one-page.pdf | - | - | - | skip | no golden for --static-id |
| encrypted-r4-three-page.pdf | - | - | - | skip | no golden for --static-id |

# Static-ID Baseline (flpdf vs qpdf --deterministic-id)

| fixture | flpdf bytes | golden bytes | verdict | first-diff |
|---|---|---|---|---|
| one-page.pdf | 1670 | 1189 | diverge | length mismatch (flpdf=1670 golden=1189) |
| two-page.pdf | 2149 | 1579 | diverge | length mismatch (flpdf=2149 golden=1579) |
| three-page.pdf | 2628 | 1970 | diverge | length mismatch (flpdf=2628 golden=1970) |
| attachment-two-page.pdf | 2581 | 2153 | diverge | length mismatch (flpdf=2581 golden=2153) |
| linearized-one-page.pdf | - | - | skip | no golden for --static-id |
| encrypted-r4-three-page.pdf | - | - | skip | no golden for --static-id |

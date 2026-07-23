# Static-ID Baseline (flpdf vs qpdf --static-id)

`flpdf-sha` is a stable 64-bit FNV-1a fingerprint of flpdf's output.
Changes to flpdf output (even those that keep the verdict label and the
length / first-diff summary the same) flip this column and fail the
baseline test, so silent drift is caught.

| fixture | flpdf-sha | flpdf bytes | golden bytes | verdict | first-diff |
|---|---|---|---|---|---|
| one-page.pdf | 4cc165113787127f | 1189 | 1189 | match | - |
| two-page.pdf | dfabd8d79a7dc82f | 1579 | 1579 | match | - |
| three-page.pdf | 3da3a9aa45213958 | 1970 | 1970 | match | - |
| attachment-two-page.pdf | 41dc413b58e1e668 | 2153 | 2153 | match | - |
| linearized-one-page.pdf | - | - | - | skip | no golden for --static-id |
| encrypted-r4-three-page.pdf | - | - | - | skip | no golden for --static-id |

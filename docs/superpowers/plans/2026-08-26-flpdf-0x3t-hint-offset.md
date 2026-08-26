# flpdf-0x3t implementation plan

1. Add RED regressions in `linearization/show.rs` for cached and uncached
   non-stream hint objects whose trailing token is shorter than `endobj`.
2. Thread trailing-token metadata through `reader/resolver.rs` and expose a
   crate-private resolver/Pdf operation that follows qpdf's cached-versus-new
   `damagedPDF` offset behavior.
3. Replace the fixed-width arithmetic in `linearization/check.rs` with the
   resolver-provided offset and make the new regressions GREEN.
4. Update source-near correspondence comments and run the focused tests plus
   the full local quality/coverage gates.
5. Rebase onto the latest `origin/main`, push the feature branch, create a
   Draft PR, wait for all CI and review checks, then mark it ready. Integration
   owns merging.

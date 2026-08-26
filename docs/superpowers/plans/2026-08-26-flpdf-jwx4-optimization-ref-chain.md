# Implementation plan: flpdf-jwx4 optimization redirect chains

## 1. Baseline and RED

- Run `cargo test -p flpdf optimization::tests` on the dedicated worktree.
- Add a focused multi-hop `Pdf::set_object` regression that expects every
  redirect owner and its terminal target in the page object-user set.
- Add a boundary regression proving a redirected non-top `/Page` is not
  crossed or recorded.
- Run the focused tests and capture the pre-fix failure.

## 2. GREEN implementation

[provisional — settled by TDD, not by this document]

Extend `Pending`/the traversal in `Optimization::update_object_maps` with a
canonical redirect branch. Resolve the next `ObjectRef` through `Pdf`, record
the current indirect owner once, and enqueue the target with the same user/top
context. Let the target dequeue apply the existing non-top page boundary, and
carry the original array-edge signal only far enough to preserve the old
null-reference predicate. Preserve the visited set and indirect inline-depth
reset. Do not alter parsed-file null filtering.

[/provisional]

Run the focused tests and the existing optimization test module until GREEN.

## 3. Verification

- `cargo fmt --all -- --check`
- `cargo test -p flpdf optimization::tests`
- `cargo test -p flpdf --test cmp_linearize_tests`
- `cargo test -p flpdf --test cmp_linearize_objstm_tests`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items`
- `cargo test --workspace --all-features`
- `python3 scripts/qpdf-module-docs.py --check`
- `python3 scripts/check-qpdf-deviation-markers.py`
- `scripts/patch-coverage.sh --base origin/main`

Before delivery, rebase onto the latest `origin/main`, rerun the affected
checks, push the branch, open a Draft PR, and mark it ready only after all
required CI and review-thread checks are green.

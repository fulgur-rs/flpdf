#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd -P)"
fixture_root="$(mktemp -d)"
trap 'rm -rf -- "${fixture_root}"' EXIT

make_fixture_repo() {
  local name="$1"
  local source_path="$2"
  local source_body="$3"
  local fixture_repo="${fixture_root}/${name}"

  mkdir -p "$(dirname "${fixture_repo}/${source_path}")" "${fixture_repo}/scripts"
  cp -f "${repo_root}/scripts/patch-coverage.sh" \
    "${fixture_repo}/scripts/patch-coverage.sh"
  chmod +x "${fixture_repo}/scripts/patch-coverage.sh"

  git -C "${fixture_repo}" init -q
  git -C "${fixture_repo}" config user.email contract@example.invalid
  git -C "${fixture_repo}" config user.name contract
  printf '%s\n' base >"${fixture_repo}/README"
  git -C "${fixture_repo}" add README scripts/patch-coverage.sh
  git -C "${fixture_repo}" commit --no-gpg-sign --no-verify -qm base
  local base_commit
  base_commit="$(git -C "${fixture_repo}" rev-parse HEAD)"

  printf '%s\n' "${source_body}" >"${fixture_repo}/${source_path}"
  : >"${fixture_repo}/report.lcov"
  git -C "${fixture_repo}" add "${source_path}" report.lcov
  git -C "${fixture_repo}" commit --no-gpg-sign --no-verify -qm change

  printf '%s\n%s\n' "${fixture_repo}" "${base_commit}"
}

{
  IFS= read -r test_repo
  IFS= read -r test_base
} < <(
  make_fixture_repo \
    test-only \
    crates/flpdf/src/json/input_tests.rs \
    $'#[test]\nfn exercised_by_test_harness() {}'
)
if ! test_output="$(
  cd "${test_repo}"
  scripts/patch-coverage.sh --base "${test_base}" --lcov report.lcov 2>&1
)"; then
  printf '%s\n' "${test_output}" >&2
  echo "patch coverage should ignore test-only source files" >&2
  exit 1
fi
[[ "${test_output}" == *"PASS (no executable changed lines)"* ]]

{
  IFS= read -r declaration_repo
  IFS= read -r declaration_base
} < <(
  make_fixture_repo \
    declaration-only \
    crates/flpdf/src/encryption/keys.rs \
    $'//! qpdf correspondence\npub enum ObjectKeyAlg {\n    Rc4,\n    Aes,\n}'
)
if ! declaration_output="$(
  cd "${declaration_repo}"
  scripts/patch-coverage.sh --base "${declaration_base}" --lcov report.lcov 2>&1
)"; then
  printf '%s\n' "${declaration_output}" >&2
  echo "patch coverage should ignore files containing only type declarations" >&2
  exit 1
fi
[[ "${declaration_output}" == *"PASS (no executable changed lines)"* ]]

{
  IFS= read -r production_repo
  IFS= read -r production_base
} < <(
  make_fixture_repo \
    production \
    crates/flpdf/src/json/input.rs \
    $'pub fn uncovered_production_line() {}'
)
if production_output="$(
  cd "${production_repo}"
  scripts/patch-coverage.sh --base "${production_base}" --lcov report.lcov 2>&1
)"; then
  printf '%s\n' "${production_output}" >&2
  echo "patch coverage should reject unmeasured production source files" >&2
  exit 1
fi
[[ "${production_output}" == *"no coverage data for changed flpdf files"* ]]

#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
qpdf_source="$(
  cd "$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
  pwd -P
)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"
build_dir=

path_is_within() {
  local child="$1"
  local parent="$2"
  [[ "${child}" == "${parent}" || "${child}" == "${parent}/"* ]]
}

cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "${build_dir}" && -d "${build_dir}" ]]; then
    rm -rf -- "${build_dir}"
  fi
  if [[ -n "$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)" ]]; then
    echo "qpdf-rc4-diff.sh: probe modified pinned tracked source" >&2
    status=1
  fi
  exit "${status}"
}
trap cleanup EXIT

temp_base="${TMPDIR:-/tmp}"
if [[ -L "${temp_base}" || ! -d "${temp_base}" ]]; then
  echo "qpdf-rc4-diff.sh: TMPDIR is not a real directory" >&2
  exit 1
fi
temp_base="$(cd "${temp_base}" && pwd -P)"
if path_is_within "${temp_base}" "${repo_root}" ||
  path_is_within "${temp_base}" "${qpdf_source}"; then
  echo "qpdf-rc4-diff.sh: TMPDIR must be outside the repository and pinned source" >&2
  exit 1
fi
build_dir="$(mktemp -d "${temp_base}/flpdf-qpdf-rc4.XXXXXXXX")"
build_dir="$(cd "${build_dir}" && pwd -P)"
if path_is_within "${build_dir}" "${repo_root}" ||
  path_is_within "${build_dir}" "${qpdf_source}" ||
  [[ "$(stat -c '%u' -- "${build_dir}")" != "${UID}" ]] ||
  [[ "$(stat -c '%a' -- "${build_dir}")" != 700 ]]; then
  echo "qpdf-rc4-diff.sh: build directory is not private and external" >&2
  exit 1
fi

if [[ "$(git -C "${qpdf_source}" rev-parse HEAD)" != "${qpdf_commit}" ]]; then
  echo "qpdf-rc4-diff.sh: pinned source is not at ${qpdf_commit}" >&2
  exit 1
fi
if [[ -n "$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)" ]]; then
  echo "qpdf-rc4-diff.sh: pinned source has tracked-file changes" >&2
  exit 1
fi

probe="${build_dir}/qpdf_rc4_probe"
c++ -std=c++17 \
  -I"${qpdf_source}/libqpdf" \
  -I"${qpdf_source}/include" \
  "${repo_root}/tests/oracle/qpdf_rc4_probe.cc" \
  "${qpdf_source}/libqpdf/RC4_native.cc" \
  -o "${probe}"

cd "${repo_root}"
QPDF_RC4_PROBE="${probe}" \
  cargo test -p flpdf --lib \
  security::rc4::tests::qpdf_rc4_differential -- --ignored --exact

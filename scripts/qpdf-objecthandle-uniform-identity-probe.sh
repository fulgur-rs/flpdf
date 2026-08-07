#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" != 0 ]]; then
  echo "usage: qpdf-objecthandle-uniform-identity-probe.sh" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
qpdf_source="$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
qpdf_source="$(cd "${qpdf_source}" && pwd -P)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"

check_source_pin() {
  local actual_commit
  actual_commit="$(git -C "${qpdf_source}" rev-parse --verify HEAD)"
  if [[ "${actual_commit}" != "${qpdf_commit}" ]]; then
    echo "qpdf-objecthandle-uniform-identity-probe.sh: pinned source commit mismatch" >&2
    return 1
  fi
}

check_source_clean() {
  local source_status
  source_status="$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)"
  if [[ -n "${source_status}" ]]; then
    echo "qpdf-objecthandle-uniform-identity-probe.sh: pinned source has tracked-file changes" >&2
    return 1
  fi
}

check_source_pin
check_source_clean

build_dir="$(mktemp -d -t flpdf-qpdf-objecthandle-uniform-XXXXXXXX)"
build_dir="$(realpath -e -- "${build_dir}")"
case "${build_dir}" in
  /tmp/flpdf-qpdf-objecthandle-uniform-*) ;;
  *)
    echo "qpdf-objecthandle-uniform-identity-probe.sh: unsafe build directory" >&2
    exit 1
    ;;
esac
cleanup() {
  case "${build_dir:-}" in
    /tmp/flpdf-qpdf-objecthandle-uniform-*) rm -rf -- "${build_dir}" ;;
  esac
}
trap cleanup EXIT

cmake -S "${qpdf_source}" -B "${build_dir}" \
  -DBUILD_STATIC_LIBS=OFF \
  -DBUILD_SHARED_LIBS=ON \
  -DREQUIRE_CRYPTO_NATIVE=OFF \
  -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "${build_dir}" --target libqpdf --parallel >/dev/null

probe_binary="${build_dir}/qpdf-objecthandle-uniform-identity-probe"
c++ -std=c++17 \
  -DPOINTERHOLDER_TRANSITION=4 \
  -I"${qpdf_source}/include" \
  -I"${qpdf_source}/libqpdf" \
  "${repo_root}/tests/oracle/qpdf_objecthandle_uniform_identity_probe.cc" \
  -L"${build_dir}/libqpdf" \
  -Wl,--disable-new-dtags \
  "-Wl,-rpath,${build_dir}/libqpdf" \
  -lqpdf \
  -o "${probe_binary}"

check_source_pin
check_source_clean
probe_lib_dir="$(cd "${build_dir}/libqpdf" && pwd -P)"
probe_library_path="${probe_lib_dir}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
loader_output="$(LD_LIBRARY_PATH="${probe_library_path}" ldd "${probe_binary}")"
resolved_libqpdf="$(awk '$1 ~ /^libqpdf\.so/ && $2 == "=>" { print $3; exit }' <<<"${loader_output}")"
resolved_libqpdf="$(realpath -e -- "${resolved_libqpdf}")"
case "${resolved_libqpdf}" in
  "${probe_lib_dir}"/*) ;;
  *)
    echo "qpdf-objecthandle-uniform-identity-probe.sh: probe resolved an unpinned libqpdf" >&2
    exit 1
    ;;
esac

LD_LIBRARY_PATH="${probe_library_path}" "${probe_binary}"
check_source_pin
check_source_clean
